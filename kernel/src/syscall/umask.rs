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

//! The `umask` syscall is used to set the process's file creation mask.

use crate::{file, process::Process, syscall::Args};
use core::mem;
use utils::{
	errno::{EResult, Errno},
	lock::{IntMutex, IntMutexGuard},
	ptr::arc::Arc,
};

pub fn umask(Args(mask): Args<file::Mode>, proc: Arc<IntMutex<Process>>) -> EResult<usize> {
	let prev = mem::replace(&mut proc.lock().umask, mask & 0o777);
	Ok(prev as _)
}
