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

//! A gap is a region of the virtual memory which is available for allocation.

use crate::memory::VirtAddr;
use core::{cmp::min, fmt, num::NonZeroUsize};
use utils::limits::PAGE_SIZE;

/// A gap in the memory space that can use for new mappings.
#[derive(Clone)]
pub struct MemGap {
	/// Address on the virtual memory to the beginning of the gap
	begin: VirtAddr,
	/// The size of the gap in pages.
	size: NonZeroUsize,
}

impl MemGap {
	/// Creates a new instance.
	///
	/// Arguments:
	/// - `begin` is a pointer on the virtual memory to the beginning of the gap. This pointer must
	///   be page-aligned.
	/// - `size` is the size of the gap in pages.
	pub fn new(begin: VirtAddr, size: NonZeroUsize) -> Self {
		debug_assert!(begin.is_aligned_to(PAGE_SIZE));
		Self {
			begin,
			size,
		}
	}

	/// Returns a pointer on the virtual memory to the beginning of the gap.
	#[inline]
	pub fn get_begin(&self) -> VirtAddr {
		self.begin
	}

	/// Returns a pointer on the virtual memory to the end of the gap.
	#[inline]
	pub fn get_end(&self) -> VirtAddr {
		self.begin + self.size.get() * PAGE_SIZE
	}

	/// Returns the size of the gap in memory pages.
	#[inline]
	pub fn get_size(&self) -> NonZeroUsize {
		self.size
	}

	/// Returns the offset in pages to the given address in the gap.
	#[inline]
	pub fn get_page_offset_for(&self, addr: VirtAddr) -> usize {
		(addr.0 - self.begin.0) / PAGE_SIZE
	}

	/// Creates new gaps to replace the current one after mapping memory onto
	/// it.
	///
	/// Arguments:
	/// - `off` is the offset of the part to consume
	/// - `size` is the size of the part to consume
	///
	/// The function returns a new gap. If the gap is fully consumed, the
	/// function returns `(None, None)`.
	pub fn consume(&self, off: usize, size: usize) -> (Option<Self>, Option<Self>) {
		// The new gap located before the mapping
		let left = NonZeroUsize::new(off).map(|off| Self {
			begin: self.begin,
			size: min(off, self.size),
		});
		// The new gap located after the mapping
		let right = self
			.size
			.get()
			.checked_sub(off + size)
			.and_then(NonZeroUsize::new)
			.map(|gap_size| Self {
				begin: self.begin + (off + size) * PAGE_SIZE,
				size: gap_size,
			});
		(left, right)
	}

	/// Merges the given gap `other` with the current gap.
	///
	/// If the gaps are not adjacent, the function does nothing.
	pub fn merge(&mut self, other: &Self) {
		if self.begin == other.get_end() {
			// If `other` is before
			self.begin = other.begin;
			self.size = self.size.saturating_add(other.size.get());
		} else if self.get_end() == other.begin {
			// If `other` is after
			self.size = self.size.saturating_add(other.size.get());
		}
	}
}

impl fmt::Debug for MemGap {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("MemGap")
			.field("begin", &self.begin)
			.field("end", &self.get_end())
			.finish()
	}
}
