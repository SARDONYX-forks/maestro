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

//! The meminfo file, allows to retrieve information about memory usage of the system.

use crate::memory;
use core::cmp::min;
use utils::{errno, errno::EResult, format, io::IO};

/// The meminfo file.
#[derive(Debug)]
pub struct MemInfo {}

impl IO for MemInfo {
	fn get_size(&self) -> u64 {
		0
	}

	fn read(&mut self, offset: u64, buff: &mut [u8]) -> EResult<(u64, bool)> {
		if buff.is_empty() {
			return Ok((0, false));
		}

		// Generate content
		let mem_info = memory::stats::MEM_INFO.lock();
		let content = format!("{}", *mem_info)?;

		// Copy content to userspace buffer
		let content_bytes = content.as_bytes();
		let len = min((content_bytes.len() as u64 - offset) as usize, buff.len());
		buff[..len].copy_from_slice(&content_bytes[(offset as usize)..(offset as usize + len)]);

		let eof = (offset + len as u64) >= content_bytes.len() as u64;
		Ok((len as _, eof))
	}

	fn write(&mut self, _offset: u64, _buff: &[u8]) -> EResult<u64> {
		Err(errno!(EINVAL))
	}

	fn poll(&mut self, _mask: u32) -> EResult<u32> {
		// TODO
		todo!();
	}
}
