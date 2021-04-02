/// This module implements the MemSpace structure which is responsible for handling the memory
/// mapping of execution contexts.
/// TODO doc

mod gap;
mod mapping;
mod physical_ref_counter;

use core::cmp::Ordering;
use core::cmp::min;
use core::ffi::c_void;
use core::ptr::NonNull;
use crate::errno::Errno;
use crate::errno;
use crate::memory::vmem::VMem;
use crate::memory::vmem;
use crate::memory;
use crate::util::FailableClone;
use crate::util::boxed::Box;
use crate::util::container::binary_tree::BinaryTree;
use crate::util::container::binary_tree;
use crate::util::list::List;
use crate::util::lock::mutex::Mutex;
use crate::util::math;
use gap::MemGap;
use mapping::MemMapping;
use physical_ref_counter::PhysRefCounter;

/// Flag telling that a memory mapping can be written to.
pub const MAPPING_FLAG_WRITE: u8  = 0b00001;
/// Flag telling that a memory mapping can contain executable instructions.
pub const MAPPING_FLAG_EXEC: u8   = 0b00010;
/// Flag telling that a memory mapping is accessible from userspace.
pub const MAPPING_FLAG_USER: u8   = 0b00100;
/// Flag telling that a memory mapping must allocate its physical memory right away and not when
/// the process tries to write to it.
pub const MAPPING_FLAG_NOLAZY: u8 = 0b01000;
/// Flag telling that a memory mapping has its physical memory shared with one or more other
/// mappings.
pub const MAPPING_FLAG_SHARED: u8 = 0b10000;

/// The number of buckets for available gaps in memory.
const GAPS_BUCKETS_COUNT: usize = 8;

/// The physical pages reference counter.
pub static mut PHYSICAL_REF_COUNTER: Mutex::<PhysRefCounter> = Mutex::new(PhysRefCounter::new());

/// Structure representing the virtual memory space of a context.
pub struct MemSpace {
	/// Binary tree storing the list of memory gaps, ready for new mappings. Sorted by pointer to
	/// the beginning of the mapping on the virtual memory.
	gaps: BinaryTree::<MemGap>,
	/// The gaps bucket, sorted by size. The minimum size in pages of a gap is: `2^^n`, where `n`
	/// is the index in the list.
	gaps_buckets: [List::<MemGap>; GAPS_BUCKETS_COUNT],

	/// Binary tree storing the list of memory mappings. Sorted by pointer to the beginning of the
	/// mapping on the virtual memory.
	mappings: BinaryTree::<MemMapping>,

	/// The virtual memory context handler.
	vmem: Box::<dyn VMem>,
}

impl MemSpace {
	/// Returns the bucket index for a gap of size `size`.
	fn get_gap_bucket_index(size: usize) -> usize {
		min(math::log2(size), GAPS_BUCKETS_COUNT - 1)
	}

	/// Inserts the given gap into the memory space's structures.
	fn gap_insert(&mut self, gap: MemGap) -> Result<(), Errno> {
		let gap_ptr = gap.get_begin();
		self.gaps.insert(gap)?;
		let g = self.gaps.get_mut(gap_ptr).unwrap();

		let bucket_index = Self::get_gap_bucket_index(g.get_size());
		let bucket = &mut self.gaps_buckets[bucket_index];
		bucket.insert_front(&mut g.list);

		Ok(())
	}

	/// Removes the given gap from the memory space's structures.
	fn gap_remove(&mut self, gap_begin: *const c_void) {
		let g = self.gaps.get_mut(gap_begin).unwrap();

		let bucket_index = Self::get_gap_bucket_index(g.get_size());
		let bucket = &mut self.gaps_buckets[bucket_index];
		g.list.unlink_from(bucket);

		self.gaps.remove(gap_begin);
	}

	/// Returns a reference to a gap with at least size `size`.
	fn gap_get(buckets: &mut [List::<MemGap>], size: usize) -> Option::<&mut MemGap> {
		let bucket_index = Self::get_gap_bucket_index(size);

		for i in bucket_index..GAPS_BUCKETS_COUNT {
			let bucket = &mut buckets[i];

			let mut node = bucket.get_front();
			while node.is_some() {
				let n = node.unwrap();
				let value = n.get_mut::<MemGap>(bucket.get_inner_offset());
				if value.get_size() >= size {
					return Some(value);
				}
				node = n.get_next();
			}
		}

		None
	}

	/// Returns a new binary tree containing the default gaps for a memory space.
	fn create_default_gaps(&mut self) -> Result::<(), Errno> {
		let begin = memory::ALLOC_BEGIN;
		let size = (memory::PROCESS_END as usize - begin as usize) / memory::PAGE_SIZE;
		self.gap_insert(MemGap::new(begin, size))
	}

	/// Creates a new virtual memory object.
	pub fn new() -> Result::<Self, Errno> {
		let mut s = Self {
			gaps: BinaryTree::new(),
			gaps_buckets: [crate::list_new!(MemGap, list); GAPS_BUCKETS_COUNT],

			mappings: BinaryTree::new(),

			vmem: vmem::new()?,
		};
		s.create_default_gaps()?;
		Ok(s)
	}

	/// Returns a mutable reference to the vvirtual memory context.
	pub fn get_vmem(&mut self) -> &mut Box::<dyn VMem> {
		&mut self.vmem
	}

	/// Maps a region of memory.
	/// `ptr` represents the address of the beginning of the region on the virtual memory.
	/// If the address is None, the function shall find a gap in the memory space that is large
	/// enough to contain the mapping.
	/// `size` represents the size of the region in number of memory pages.
	/// `flags` represents the flags for the mapping.
	/// underlying physical memory is not allocated directly but only an attempt to write the
	/// memory is detected.
	/// The function returns a pointer to the newly mapped virtual memory.
	pub fn map(&mut self, ptr: Option::<*const c_void>, size: usize, flags: u8)
		-> Result<*const c_void, Errno> {
		if let Some(_ptr) = ptr {
			// TODO Insert mapping at exact location if possible
			Err(errno::ENOMEM)
		} else {
			let gap = Self::gap_get(&mut self.gaps_buckets, size);
			if gap.is_none() {
				return Err(errno::ENOMEM);
			}

			let gap = gap.unwrap();
			let gap_ptr = gap.get_begin();

			let mapping = MemMapping::new(gap_ptr, size, flags,
				NonNull::new(self.vmem.as_mut_ptr()).unwrap());
			let mapping_ptr = mapping.get_begin();
			self.mappings.insert(mapping)?;

			if self.mappings.get_mut(mapping_ptr).unwrap().map_default().is_err() {
				self.mappings.remove(mapping_ptr);
				return Err(errno::ENOMEM);
			}

			if let Some(new_gap) = gap.consume(size) {
				if self.gap_insert(new_gap).is_err() {
					self.mappings.get_mut(mapping_ptr).unwrap().unmap();
					self.mappings.remove(mapping_ptr);
					return Err(errno::ENOMEM);
				}
			}

			self.gap_remove(gap_ptr);
			Ok(mapping_ptr)
		}
	}

	/// Same as `map`, except the function returns a pointer to the end of the memory region.
	pub fn map_stack(&mut self, ptr: Option::<*const c_void>, size: usize, flags: u8)
		-> Result<*const c_void, Errno> {
		let mapping_ptr = self.map(ptr, size, flags)?;
		Ok(unsafe { // Call to unsafe function
			mapping_ptr.add(size * memory::PAGE_SIZE)
		})
	}

	/// Returns a mutable reference to the memory mapping containing the given virtual address
	/// `ptr` from mappings container `mappings`. If no mapping contains the address, the function
	/// returns None.
	fn get_mapping_for(mappings: &mut BinaryTree::<MemMapping>, ptr: *const c_void)
		-> Option::<&mut MemMapping> {
		mappings.cmp_get(| val | {
			let begin = val.get_begin();
			let end = (begin as usize + val.get_size() * memory::PAGE_SIZE) as *const c_void;
			if ptr >= begin && ptr < end {
				Ordering::Equal
			} else if ptr < begin {
				Ordering::Less
			} else {
				Ordering::Greater
			}
		})
	}

	/// Unmaps the given region of memory.
	/// `ptr` represents the address of the beginning of the region on the virtual memory.
	/// `size` represents the size of the region in number of memory pages.
	/// The function frees the physical memory the region points to unless shared by one or several
	/// other memory mappings.
	/// After this function returns, the access to the region of memory shall be revoked and
	/// further attempts to access it shall result in a page fault.
	pub fn unmap(&mut self, _ptr: *const c_void, _size: usize) {
		// TODO
	}

	/// Binds the CPU to this memory space.
	pub fn bind(&self) {
		self.vmem.bind();
	}

	/// Clones the current memory space for process forking.
	pub fn fork(&mut self) -> Result<MemSpace, Errno> {
		let mut mem_space = Self {
			gaps: BinaryTree::new(),
			gaps_buckets: [crate::list_new!(MemGap, list); GAPS_BUCKETS_COUNT],

			mappings: BinaryTree::new(),

			vmem: vmem::clone(&self.vmem)?,
		};

		for g in self.gaps.into_iter() {
			let new_gap = g.failable_clone()?;
			mem_space.gap_insert(new_gap)?;
		}

		for m in self.mappings.iter_mut() {
			let new_mapping = m.fork(&mut mem_space.mappings)?;
			for i in 0..new_mapping.get_size() {
				if new_mapping.get_flags() & MAPPING_FLAG_NOLAZY == 0 {
					new_mapping.update_vmem(i);
				} else {
					new_mapping.map(i)?;
				}
			}
		}
		for m in self.mappings.iter_mut() {
			for i in 0..m.get_size() {
				m.update_vmem(i);
			}
		}

		Ok(mem_space)
	}

	/// Function called whenever the CPU triggered a page fault for the context. This function
	/// determines whether the process should continue or not. If continuing, the function must
	/// resolve the issue before returning.
	/// A typical situation where is function is usefull is for Copy-On-Write allocations.
	///
	/// `virt_addr` is the virtual address of the wrong memory access that caused the fault.
	/// `code` is the error code given along with the error.
	/// If the process should continue, the function returns `true`, else `false`.
	pub fn handle_page_fault(&mut self, virt_addr: *const c_void, code: u32) -> bool {
		if code & vmem::x86::PAGE_FAULT_PRESENT == 0 {
			return false;
		}

		let mapping = Self::get_mapping_for(&mut self.mappings, virt_addr).unwrap();
		let offset = (virt_addr as usize - mapping.get_begin() as usize) / memory::PAGE_SIZE;
		if mapping.map(offset).is_err() {
			// TODO Use OOM-killer
			// TODO Check if current process has been killed
			if mapping.map(offset).is_err() {
				crate::kernel_panic!("OOM killer is unable to free up space for new allocation!");
			}
		}

		true
	}
}

impl Drop for MemSpace {
	fn drop(&mut self) {
		self.mappings.foreach(| m | {
			m.unmap();
		}, binary_tree::TraversalType::PreOrder);
	}
}
