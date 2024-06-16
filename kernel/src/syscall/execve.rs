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

//! The `execve` system call allows to execute a program from a file.

use crate::{
	file::{
		path::{Path, PathBuf},
		vfs,
		vfs::ResolutionSettings,
		File,
	},
	memory::stack,
	process::{
		exec,
		exec::{ExecInfo, ProgramImage},
		mem_space::ptr::{SyscallArray, SyscallString},
		regs::Regs,
		scheduler::SCHEDULER,
		Process,
	},
};
use macros::syscall;
use utils::{
	collections::{string::String, vec::Vec},
	errno,
	errno::{CollectResult, EResult, Errno},
	interrupt::cli,
	io::IO,
	lock::Mutex,
	ptr::arc::Arc,
};

/// The maximum length of the shebang.
const SHEBANG_MAX: usize = 256;
/// The maximum number of interpreters that can be used recursively for an
/// execution.
const INTERP_MAX: usize = 4;

// TODO Use ARG_MAX

/// Returns the file for the given `path`.
///
/// The function also parses and evential shebang string and builds the resulting **argv**.
///
/// Arguments:
/// - `path` is the path of the executable file.
/// - `rs` is the resolution settings to be used to open files.
/// - `argv` is an iterator over the arguments passed to the system call.
fn get_file<'a, A: Iterator<Item = EResult<&'a [u8]>> + 'a>(
	path: &Path,
	rs: &ResolutionSettings,
	argv: A,
) -> EResult<(Arc<Mutex<File>>, Vec<String>)> {
	let mut buffers: [[u8; SHEBANG_MAX]; INTERP_MAX] = [[0; SHEBANG_MAX]; INTERP_MAX];
	// Read and parse shebangs
	let mut file_mutex = vfs::get_file_from_path(path, rs)?;
	let mut i = 0;
	loop {
		// If there is still an interpreter but the limit has been reached
		if i >= INTERP_MAX {
			return Err(errno!(ELOOP));
		}
		let buff = &mut buffers[i];
		// Read file
		let len = {
			let mut file = file_mutex.lock();
			// Check permission
			if !rs.access_profile.can_execute_file(&file) {
				return Err(errno!(EACCES));
			}
			file.read(0, buff)?.0 as usize
		};
		// Parse shebang
		let end = buff[..len].iter().position(|b| *b == b'\n').unwrap_or(len);
		if !matches!(buff[..end], [b'#', b'!', _, ..]) {
			break;
		}
		i += 1;
		// Get interpreter path
		let interp_end = buff[2..end]
			.iter()
			.position(|b| (*b as char).is_ascii_whitespace())
			.unwrap_or(end);
		let interp_path = Path::new(&buff[2..interp_end])?;
		// Read interpreter
		file_mutex = vfs::get_file_from_path(&interp_path, rs)?;
	}
	// Build arguments
	let final_argv = buffers[..=i]
		.into_iter()
		.rev()
		.enumerate()
		.flat_map(|(i, shebang)| {
			let mut words = shebang[2..]
				.split(|b| (*b as char).is_ascii_whitespace())
				.map(|s| Ok(String::try_from(s)?));
			// Skip interpreters, except the first
			if i > 0 {
				words.next();
			}
			words
		})
		.chain(argv.map(|s| s.and_then(|s| Ok(String::try_from(s)?))))
		.collect::<EResult<CollectResult<Vec<String>>>>()?
		.0?;
	Ok((file_mutex, final_argv))
}

/// Performs the execution on the current process.
fn do_exec(
	file: Arc<Mutex<File>>,
	rs: &ResolutionSettings,
	argv: Vec<String>,
	envp: Vec<String>,
) -> EResult<Regs> {
	let program_image = build_image(file, rs, argv, envp)?;
	let proc_mutex = Process::current_assert();
	let mut proc = proc_mutex.lock();
	// Execute the program
	exec::exec(&mut proc, program_image)?;
	Ok(proc.regs.clone())
}

/// Builds a program image.
///
/// Arguments:
/// - `file` is the executable file
/// - `path_resolution` is settings for path resolution
/// - `argv` is the arguments list
/// - `envp` is the environment variables list
fn build_image(
	file: Arc<Mutex<File>>,
	path_resolution: &ResolutionSettings,
	argv: Vec<String>,
	envp: Vec<String>,
) -> EResult<ProgramImage> {
	let mut file = file.lock();
	if !path_resolution.access_profile.can_execute_file(&file) {
		return Err(errno!(EACCES));
	}
	let exec_info = ExecInfo {
		path_resolution,
		argv,
		envp,
	};
	exec::build_image(&mut file, exec_info)
}

#[syscall]
pub fn execve(pathname: SyscallString, argv: SyscallArray, envp: SyscallArray) -> EResult<i32> {
	let (file, rs, argv, envp) = {
		let proc_mutex = Process::current_assert();
		let proc = proc_mutex.lock();

		let mem_space = proc.get_mem_space().unwrap();
		let mem_space_guard = mem_space.lock();

		let path = pathname
			.get(&mem_space_guard)?
			.ok_or_else(|| errno!(EFAULT))?;
		let path = PathBuf::try_from(path)?;

		let rs = ResolutionSettings::for_process(&proc, true);
		let argv = argv.iter(&mem_space_guard);
		let (file, argv) = get_file(&path, &rs, argv)?;
		let envp = envp
			.iter(&mem_space_guard)
			.map(|s| s.and_then(|s| Ok(String::try_from(s)?)))
			.collect::<EResult<CollectResult<Vec<_>>>>()?
			.0?;
		(file, rs, argv, envp)
	};
	// Disable interrupt to prevent stack switching while using a temporary stack,
	// preventing this temporary stack from being used as a signal handling stack
	cli();
	let tmp_stack = SCHEDULER.get().lock().get_tmp_stack();
	let exec = move || {
		let regs = do_exec(file, &rs, argv, envp)?;
		unsafe {
			regs.switch(true);
		}
	};
	unsafe { stack::switch(tmp_stack as _, exec) }
}
