/*
 * Copyright 2024 Luc Lenôtre
 *
 * This file is part of Maestro.
 *
 * Maestro is free software: you can redistribute it and/or modify it under the
 * terms of the GNU General Public License as published by the Free Software
 * Foundation, either version 3 of the License, or (at your option) any later
 * version.
 *
 * Maestro is distributed in the hope that it will be useful, but WITHOUT ANY
 * WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
 * A PARTICULAR PURPOSE. See the GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License along with
 * Maestro. If not, see <https://www.gnu.org/licenses/>.
 */

//! Storage management implementation.

pub mod ide;
pub mod partition;
pub mod pata;
pub mod ramdisk;

use crate::{
	device,
	device::{
		bus::pci,
		id,
		id::MajorBlock,
		manager::{DeviceManager, PhysicalDevice},
		Device, DeviceID, DeviceIO, DeviceType,
	},
	file::{
		path::{Path, PathBuf},
		Mode,
	},
	process::mem_space::copy::SyscallPtr,
	syscall::{ioctl, FromSyscallArg},
};
use core::ffi::{c_uchar, c_ulong, c_ushort, c_void};
use partition::Partition;
use utils::{
	collections::vec::Vec,
	errno,
	errno::EResult,
	format,
	lock::Mutex,
	ptr::arc::{Arc, Weak},
	TryClone,
};

/// The major number for storage devices.
const STORAGE_MAJOR: u32 = 8;
/// The mode of the device file for a storage device.
const STORAGE_MODE: Mode = 0o660;
/// The maximum number of partitions in a disk.
const MAX_PARTITIONS: usize = 16;

/// Hard drive geometry.
#[derive(Debug)]
#[repr(C)]
struct HdGeometry {
	/// The number of heads (CHS).
	heads: c_uchar,
	/// The number of sectors (CHS).
	sectors: c_uchar,
	/// The number of cylinders (CHS).
	cylinders: c_ushort,
	/// Starting LBA of the device.
	start: c_ulong,
}

/// Handle for the device file of a whole storage device or a partition.
pub struct StorageDeviceHandle {
	/// Device I/O.
	io: Weak<Mutex<dyn DeviceIO>>,
	/// The partition associated with the handle. If `None`, the handle covers the whole device.
	partition: Option<Partition>,

	/// The major number of the device.
	major: u32,
	/// The ID of the storage device in the manager.
	storage_id: u32,
	/// The path to the file of the main device containing the partition table.
	path_prefix: PathBuf,
}

impl StorageDeviceHandle {
	/// Creates a new instance for the given storage interface and the given
	/// partition number.
	///
	/// Arguments:
	/// - `io` is the storage I/O interface.
	/// - `partition` is the partition. If `None`, the handle works on the whole storage device.
	/// - `major` is the major number of the device.
	/// - `storage_id` is the ID of the storage device in the manager.
	/// - `path_prefix` is the path to the file of the main device containing the partition table.
	pub fn new(
		io: Weak<Mutex<dyn DeviceIO>>,
		partition: Option<Partition>,
		major: u32,
		storage_id: u32,
		path_prefix: PathBuf,
	) -> Self {
		Self {
			io,
			partition,

			major,
			storage_id,
			path_prefix,
		}
	}
}

impl DeviceIO for StorageDeviceHandle {
	fn read(&mut self, offset: u64, buff: &mut [u8]) -> EResult<u64> {
		let Some(io) = self.io.upgrade() else {
			return Err(errno!(ENODEV));
		};
		let mut io = io.lock();
		// Check offset
		let (start, size) = match &self.partition {
			Some(p) => {
				let block_size = io.block_size().get();
				let start = p.get_offset() * block_size;
				let size = p.get_size() * block_size;
				(start, size)
			}
			None => (0, io.get_size()),
		};
		if (offset + buff.len() as u64) > size {
			return Err(errno!(EINVAL));
		}
		io.read_bytes(buff, start + offset)
	}

	fn write(&mut self, offset: u64, buff: &[u8]) -> EResult<u64> {
		let Some(io) = self.io.upgrade() else {
			return Err(errno!(ENODEV));
		};
		let mut io = io.lock();
		// Check offset
		let (start, size) = match &self.partition {
			Some(p) => {
				let block_size = io.block_size().get();
				let start = p.get_offset() * block_size;
				let size = p.get_size() * block_size;
				(start, size)
			}
			None => (0, io.get_size()),
		};
		if (offset + buff.len() as u64) > size {
			return Err(errno!(EINVAL));
		}
		io.write_bytes(buff, start + offset)
	}

	fn ioctl(&mut self, request: ioctl::Request, argp: *const c_void) -> EResult<u32> {
		match request.get_old_format() {
			ioctl::HDIO_GETGEO => {
				// The total size of the disk
				let size = {
					if let Some(io) = self.io.upgrade() {
						let io = io.lock();
						io.block_size().get() * io.blocks_count()
					} else {
						0
					}
				};

				// Translate from LBA to CHS
				let s = (size % c_uchar::MAX as u64) as _;
				let h = ((size - s as u64) / c_uchar::MAX as u64 % c_uchar::MAX as u64) as _;
				let c = ((size - s as u64) / c_uchar::MAX as u64 / c_uchar::MAX as u64) as _;

				// Starting LBA of the partition
				let start = self.partition.as_ref().map(|p| p.get_offset()).unwrap_or(0) as _;

				let hd_geo = HdGeometry {
					heads: h,
					sectors: s,
					cylinders: c,
					start,
				};

				// Write to userspace
				let hd_geo_ptr = SyscallPtr::<HdGeometry>::from_syscall_arg(argp as usize);
				hd_geo_ptr.copy_to_user(hd_geo)?;

				Ok(0)
			}

			ioctl::BLKRRPART => {
				StorageManager::clear_partitions(self.major)?;
				StorageManager::read_partitions(
					self.io.clone(),
					self.major,
					self.storage_id,
					&self.path_prefix,
				)?;

				Ok(0)
			}

			ioctl::BLKSSZGET => {
				let blk_size = {
					if let Some(io) = self.io.upgrade() {
						let io = io.lock();
						io.block_size().get()
					} else {
						0
					}
				};

				let size_ptr = SyscallPtr::<u32>::from_syscall_arg(argp as usize);
				size_ptr.copy_to_user(blk_size as _)?;

				Ok(0)
			}

			ioctl::BLKGETSIZE64 => {
				let size = {
					if let Some(io) = self.io.upgrade() {
						let io = io.lock();
						io.block_size().get() * io.blocks_count()
					} else {
						0
					}
				};

				let size_ptr = SyscallPtr::<u64>::from_syscall_arg(argp as usize);
				size_ptr.copy_to_user(size)?;

				Ok(0)
			}

			_ => Err(errno!(ENOTTY)),
		}
	}
}

/// An instance of StorageManager manages devices on a whole major number.
///
/// The manager has name `storage`.
pub struct StorageManager {
	/// The allocated device major number for storage devices.
	major_block: MajorBlock,
	/// The list of detected interfaces.
	interfaces: Vec<Arc<Mutex<dyn DeviceIO>>>,
}

impl StorageManager {
	/// Creates a new instance.
	pub fn new() -> EResult<Self> {
		Ok(Self {
			major_block: id::alloc_major(DeviceType::Block, Some(STORAGE_MAJOR))?,
			interfaces: Vec::new(),
		})
	}

	// TODO When failing, remove previously registered devices
	/// Creates device files for every partitions on the storage device, within the limit of
	/// `MAX_PARTITIONS`.
	///
	/// Arguments:
	/// - `io` is the I/O interface.
	/// - `major` is the major number of the device.
	/// - `storage_id` is the ID of the storage device in the manager.
	/// - `path_prefix` is the path to the file of the main device containing the partition table.
	pub fn read_partitions(
		io: Weak<Mutex<dyn DeviceIO>>,
		major: u32,
		storage_id: u32,
		path_prefix: &Path,
	) -> EResult<()> {
		let Some(io_mutex) = io.upgrade() else {
			return Ok(());
		};
		let mut s = io_mutex.lock();

		let Some(partitions_table) = partition::read(&mut *s)? else {
			return Ok(());
		};
		let partitions = partitions_table.get_partitions(&mut *s)?;

		let iter = partitions.into_iter().take(MAX_PARTITIONS - 1).enumerate();
		for (i, partition) in iter {
			let part_nbr = (i + 1) as u32;
			let path = PathBuf::try_from(format!("{path_prefix}{part_nbr}")?)?;

			// Create the partition's device file
			let handle = StorageDeviceHandle::new(
				io.clone(),
				Some(partition),
				major,
				storage_id,
				path_prefix.to_path_buf()?,
			);
			let device = Device::new(
				DeviceID {
					dev_type: DeviceType::Block,
					// TODO use a different major for different storage device types
					major: STORAGE_MAJOR,
					minor: storage_id * MAX_PARTITIONS as u32 + part_nbr,
				},
				path,
				STORAGE_MODE,
				handle,
			)?;
			device::register(device)?;
		}

		Ok(())
	}

	/// Clears device files for every partitions.
	///
	/// `major` is the major number of the devices to be removed.
	pub fn clear_partitions(major: u32) -> EResult<()> {
		for i in 1..MAX_PARTITIONS {
			device::unregister(&DeviceID {
				dev_type: DeviceType::Block,
				major,
				minor: i as _,
			})?;
		}

		Ok(())
	}

	// TODO Handle the case where there is more devices that the number of devices
	// that can be handled in the range of minor numbers
	// TODO When failing, remove previously registered devices
	/// Adds the given storage device to the manager.
	fn add(&mut self, storage: Arc<Mutex<dyn DeviceIO>>) -> EResult<()> {
		// The device files' major number
		let major = self.major_block.get_major();
		// The id of the storage interface in the manager's list
		let storage_id = self.interfaces.len() as u32;

		// Prefix is the path of the main device file
		// TODO Handle if out of the alphabet
		let letter = (b'a' + (storage_id as u8)) as char;
		let main_path = PathBuf::try_from(format!("/dev/sd{letter}")?)?;

		// Create the main device file
		let main_handle = StorageDeviceHandle::new(
			Arc::downgrade(&storage),
			None,
			major,
			storage_id,
			main_path.try_clone()?,
		);
		let main_device = Device::new(
			DeviceID {
				dev_type: DeviceType::Block,
				major,
				minor: storage_id * MAX_PARTITIONS as u32,
			},
			main_path.try_clone()?,
			STORAGE_MODE,
			main_handle,
		)?;
		device::register(main_device)?;

		Self::read_partitions(Arc::downgrade(&storage), major, storage_id, &main_path)?;

		self.interfaces.push(storage)?;
		Ok(())
	}

	// TODO Function to remove a device

	/// Fills a random buffer `buff` of size `size` with seed `seed`.
	///
	/// The function returns the seed for the next block.
	#[cfg(config_debug_storage_test)]
	fn random_block(size: u64, buff: &mut [u8], seed: u32) -> u32 {
		let mut s = seed;

		for i in 0..size {
			s = crate::util::math::pseudo_rand(s, 1664525, 1013904223, 0x100);
			buff[i as usize] = (s & 0xff) as u8;
		}

		s
	}

	// TODO Test with several blocks at a time
	/// Tests the given interface with the given interface `interface`.
	///
	/// `seed` is the seed for pseudo random generation. The function will set
	/// this variable to another value for the next iteration.
	#[cfg(config_debug_storage_test)]
	fn test_interface(interface: &mut dyn StorageInterface, seed: u32) -> bool {
		let block_size = interface.get_block_size();
		let blocks_count = min(1024, interface.get_blocks_count());

		let mut s = seed;
		for i in 0..blocks_count {
			let mut buff: [u8; 512] = [0; 512]; // TODO Set to block size
			s = Self::random_block(block_size, &mut buff, s);
			if interface.write(&buff, i, 1).is_err() {
				crate::println!("\nCannot write to disk on block {}.", i);
				return false;
			}
		}

		s = seed;
		for i in 0..blocks_count {
			let mut buff: [u8; 512] = [0; 512]; // TODO Set to block size
			s = Self::random_block(interface.get_block_size(), &mut buff, s);

			let mut buf: [u8; 512] = [0; 512]; // TODO Set to block size
			if interface.read(&mut buf, i, 1).is_err() {
				crate::println!("\nCannot read from disk on block {}.", i);
				return false;
			}

			if buf != buff {
				return false;
			}
		}

		true
	}

	/// Performs testing of storage devices and drivers.
	///
	/// If every tests pass, the function returns `true`. Else, it returns
	/// `false`.
	#[cfg(config_debug_storage_test)]
	fn perform_test(&mut self) -> bool {
		let mut seed = 42;
		let iterations_count = 10;
		for i in 0..iterations_count {
			let interfaces_count = self.interfaces.len();

			for j in 0..interfaces_count {
				let mut interface = self.interfaces[j].lock();

				crate::print!(
					"Processing iteration: {}/{iterations_count}; device: {}/{iterations_count}...",
					i + 1,
					j + 1,
				);

				if !Self::test_interface(&mut *interface, seed) {
					return false;
				}

				seed = crate::util::math::pseudo_rand(seed, 1103515245, 12345, 0x100);
			}

			if i < iterations_count - 1 {
				crate::print!("\r");
			} else {
				crate::println!();
			}
		}

		true
	}

	/// Tests every storage drivers on every storage devices.
	///
	/// The execution of this function removes all the data on every connected
	/// writable disks, so it must be used carefully.
	#[cfg(config_debug_storage_test)]
	pub fn test(&mut self) {
		crate::println!("Running disks tests... ({} devices)", self.interfaces.len());

		if self.perform_test() {
			crate::println!("Done!");
		} else {
			crate::println!("Storage test failed!");
		}
		crate::halt();
	}
}

impl DeviceManager for StorageManager {
	fn on_plug(&mut self, dev: &dyn PhysicalDevice) -> EResult<()> {
		// Ignore non-storage devices
		if dev.get_class() != pci::CLASS_MASS_STORAGE_CONTROLLER {
			return Ok(());
		}

		let mut register_iface = |res: EResult<_>| {
			let res = res.and_then(|iface| self.add(iface));
			if let Err(e) = res {
				crate::println!("Could not register storage device: {e}");
			}
		};

		// TODO use device class as a hint
		// TODO handle other controller types
		if let Some(ide) = ide::Controller::new(dev) {
			for iface in ide.detect() {
				register_iface(iface.map_err(Into::into));
			}
		}

		Ok(())
	}

	fn on_unplug(&mut self, _dev: &dyn PhysicalDevice) -> EResult<()> {
		// TODO remove device
		todo!();
	}
}
