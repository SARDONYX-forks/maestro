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

//! The `socketpair` system call creates a pair of file descriptor to an unnamed
//! socket which can be used for IPC (Inter-Process Communication).

use crate::{
	file::{
		buffer, buffer::socket::Socket, fd::FileDescriptorTable, open_file, open_file::OpenFile,
		perm::AccessProfile, vfs,
	},
	net::{SocketDesc, SocketDomain, SocketType},
	process::{mem_space::copy::SyscallPtr, Process},
	syscall::Args,
};
use core::ffi::c_int;
use utils::{
	errno,
	errno::{EResult, Errno},
	lock::Mutex,
	ptr::arc::Arc,
};

pub fn socketpair(
	Args((domain, r#type, protocol, sv)): Args<(c_int, c_int, c_int, SyscallPtr<[c_int; 2]>)>,
	ap: AccessProfile,
	fds: Arc<Mutex<FileDescriptorTable>>,
) -> EResult<usize> {
	let sock_domain = SocketDomain::try_from(domain as u32)?;
	let sock_type = SocketType::try_from(r#type as u32)?;
	// Check permissions
	if !ap.can_use_sock_domain(&sock_domain) || !ap.can_use_sock_type(&sock_type) {
		return Err(errno!(EACCES));
	}
	let desc = SocketDesc {
		domain: sock_domain,
		type_: sock_type,
		protocol,
	};
	// Create socket
	let sock = Socket::new(desc)?;
	let loc = buffer::register(None, sock)?;
	// Create file descriptors
	let file = vfs::get_file_from_location(loc)?;
	let open_file0 = OpenFile::new(file.clone(), None, open_file::O_RDONLY)?;
	let open_file1 = OpenFile::new(file, None, open_file::O_WRONLY)?;
	let (fd0_id, fd1_id) = fds.lock().create_fd_pair(open_file0, open_file1)?;
	sv.copy_to_user([fd0_id as _, fd1_id as _])?;
	Ok(0)
}
