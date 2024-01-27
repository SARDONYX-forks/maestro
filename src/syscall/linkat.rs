//! This `linkat` syscall creates a new hard link to a file.

use super::util::at;
use crate::errno::Errno;
use crate::file::path::Path;
use crate::file::vfs;
use crate::file::vfs::{ResolutionSettings, Resolved};
use crate::file::FileType;
use crate::process::mem_space::ptr::SyscallString;
use crate::process::Process;
use core::ffi::c_int;
use macros::syscall;

#[syscall]
pub fn linkat(
	olddirfd: c_int,
	oldpath: SyscallString,
	newdirfd: c_int,
	newpath: SyscallString,
	flags: c_int,
) -> Result<i32, Errno> {
	let (fds_mutex, oldpath, newpath, rs) = {
		let proc_mutex = Process::current_assert();
		let proc = proc_mutex.lock();

		let rs = ResolutionSettings::for_process(&*proc, false);

		let mem_space = proc.get_mem_space().unwrap().clone();
		let mem_space_guard = mem_space.lock();

		let fds_mutex = proc.file_descriptors.clone().unwrap();

		let oldpath = oldpath
			.get(&mem_space_guard)?
			.ok_or_else(|| errno!(EFAULT))?;
		let oldpath = Path::new(oldpath)?;

		let proc = proc_mutex.lock();
		let newpath = newpath
			.get(&mem_space_guard)?
			.ok_or_else(|| errno!(EFAULT))?;
		let newpath = Path::new(newpath)?;

		(fds_mutex, oldpath, newpath, rs)
	};

	let fds = fds_mutex.lock();

	let Resolved::Found(old_mutex) = at::get_file(&fds, rs, olddirfd, oldpath, flags)? else {
		return Err(errno!(ENOENT));
	};
	let mut old = old_mutex.lock();
	if matches!(old.get_type(), FileType::Directory) {
		return Err(errno!(EISDIR));
	}

	let Resolved::Creatable {
		parent: new_parent,
		name: new_name,
	} = at::get_file(&fds, rs, newdirfd, newpath, 0)?
	else {
		return Err(errno!(EEXIST));
	};
	let new_parent = new_parent.lock();

	vfs::create_link(&new_parent, &new_name, &mut old, &rs.access_profile)?;

	Ok(0)
}
