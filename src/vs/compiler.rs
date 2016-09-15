extern crate regex;
#[cfg(windows)]
extern crate winreg;

use tempdir::TempDir;

pub use ::compiler::*;

use ::vs::postprocess;
use ::utils::filter;
use ::io::memstream::MemStream;
use ::io::tempfile::TempFile;
use ::lazy::Lazy;

use std::fs::File;
use std::io::{Cursor, Error, ErrorKind, Read, Write};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use self::regex::bytes::{NoExpand, Regex};

pub struct VsCompiler {
    temp_dir: Arc<TempDir>,
    toolchains: ToolchainHolder,
}

impl VsCompiler {
    pub fn default() -> Result<Self, Error> {
        Ok(VsCompiler::new(&Arc::new(try!(TempDir::new("octobuild")))))
    }
    pub fn new(temp_dir: &Arc<TempDir>) -> Self {
        VsCompiler {
            temp_dir: temp_dir.clone(),
            toolchains: ToolchainHolder::new(),
        }
    }
}

struct VsToolchain {
    temp_dir: Arc<TempDir>,
    path: PathBuf,
    identifier: Lazy<Option<String>>,
}

impl VsToolchain {
    pub fn new(path: PathBuf, temp_dir: &Arc<TempDir>) -> Self {
        VsToolchain {
            temp_dir: temp_dir.clone(),
            path: path,
            identifier: Lazy::new(),
        }
    }
}

impl Compiler for VsCompiler {
    fn resolve_toolchain(&self, command: &CommandInfo) -> Option<Arc<Toolchain>> {
        if command.program
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase())
            .map_or(false, |n| (n == "cl.exe") || (n == "cl")) {
            command.find_executable().and_then(|path| {
                self.toolchains.resolve(&path,
                                        |path| Arc::new(VsToolchain::new(path, &self.temp_dir)))
            })
        } else {
            None
        }
    }

    #[cfg(unix)]
    fn discovery_toolchains(&self) -> Vec<Arc<Toolchain>> {
        Vec::new()
    }

    #[cfg(windows)]
    fn discovery_toolchains(&self) -> Vec<Arc<Toolchain>> {
        use self::winreg::RegKey;
        use self::winreg::enums::*;

        lazy_static!{
			static ref RE:self::regex::Regex = self::regex::Regex::new(r"^\d+\.\d+$").unwrap();
		}

        const CL_BIN: &'static [&'static str] = &["bin/cl.exe",
                                                  "bin/x86_arm/cl.exe",
                                                  "bin/x86_amd64/cl.exe",
                                                  "bin/amd64_x86/cl.exe",
                                                  "bin/amd64_arm/cl.exe",
                                                  "bin/amd64/cl.exe"];
        const VC_REG: &'static [&'static str] = &["SOFTWARE\\Wow6432Node\\Microsoft\\VisualStudio\\SxS\\VC7",
                                                  "SOFTWARE\\Microsoft\\VisualStudio\\SxS\\VC7"];

        VC_REG.iter()
            .filter_map(|reg_path| RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(reg_path, KEY_READ).ok())
            .flat_map(|key| -> Vec<String> {
                key.enum_values()
                    .filter_map(|x| x.ok())
                    .map(|(name, _)| name)
                    .filter(|name| RE.is_match(&name))
                    .filter_map(|name: String| -> Option<String> { key.get_value(name).ok() })
                    .collect()
            })
            .map(|path| Path::new(&path).to_path_buf())
            .map(|path| -> Vec<PathBuf> {
                CL_BIN.iter()
                    .map(|bin| path.join(bin))
                    .collect()
            })
            .flat_map(|paths| paths.into_iter())
            .filter(|cl| cl.exists())
            .map(|cl| -> Arc<Toolchain> { Arc::new(VsToolchain::new(cl, &self.temp_dir)) })
            .filter(|toolchain| toolchain.identifier().is_some())
            .collect()
    }
}

impl Toolchain for VsToolchain {
    fn identifier(&self) -> Option<String> {
        self.identifier.get(|| vs_identifier(&self.path))
    }

    fn create_tasks(&self, command: CommandInfo, args: &[String]) -> Result<Vec<CompilationTask>, String> {
        super::prepare::create_tasks(command, args)
    }

    fn preprocess_step(&self,
                       state: &SharedState,
                       task: &CompilationTask,
                       worker: &Fn(&Path, PreprocessResult) -> Result<(), Error>)
                       -> Result<(), Error> {
        for input_source in task.input_sources.iter() {
            // Make parameters list for preprocessing.
            let mut args = filter(&task.shared.args, |arg: &Arg| -> Option<String> {
                match arg {
                    &Arg::Flag { ref scope, ref flag } => {
                        match scope {
                            &Scope::Preprocessor |
                            &Scope::Shared => Some("/".to_string() + &flag),
                            &Scope::Ignore | &Scope::Compiler => None,
                        }
                    }
                    &Arg::Param { ref scope, ref flag, ref value } => {
                        match scope {
                            &Scope::Preprocessor |
                            &Scope::Shared => Some("/".to_string() + &flag + &value),
                            &Scope::Ignore | &Scope::Compiler => None,
                        }
                    }
                    &Arg::Input { .. } => None,
                    &Arg::Output { .. } => None,
                }
            });

            // Add preprocessor paramters.
            args.push("/nologo".to_string());
            args.push("/T".to_string() + &task.language);
            args.push("/E".to_string());
            args.push("/we4002".to_string()); // C4002: too many actual parameters for macro 'identifier'
            args.push(input_source.display().to_string());

            let mut command = task.shared.command.to_command();
            command.args(&args)
                .arg(&join_flag("/Fo", &task.output_object)); // /Fo option also set output path for #import directive
            let output = try!(state.wrap_slow(|| command.output()));
            let preprocess_result = if output.status.success() {
                let mut content = MemStream::new();
                if task.shared.input_precompiled.is_some() || task.shared.output_precompiled.is_some() {
                    try!(postprocess::filter_preprocessed(&mut Cursor::new(output.stdout),
                                                          &mut content,
                                                          &task.shared.marker_precompiled,
                                                          task.shared.output_precompiled.is_some()));
                } else {
                    try!(content.write(&output.stdout));
                };
                PreprocessResult::Success(content)
            } else {
                PreprocessResult::Failed(OutputInfo {
                    status: output.status.code(),
                    stdout: Vec::new(),
                    stderr: output.stderr,
                })
            };
            try!(worker(input_source, preprocess_result));
        }
        Ok(())
    }

    // Compile preprocessed file.
    fn compile_prepare_step(&self,
                            task: &CompilationTask,
                            input_source: &Path,
                            preprocessed: MemStream)
                            -> Result<CompileStep, Error> {
        let mut args = filter(&task.shared.args, |arg: &Arg| -> Option<String> {
            match arg {
                &Arg::Flag { ref scope, ref flag } => {
                    match scope {
                        &Scope::Compiler | &Scope::Shared => Some("/".to_string() + &flag),
                        &Scope::Preprocessor if task.shared.output_precompiled.is_some() => {
                            Some("/".to_string() + &flag)
                        }
                        &Scope::Ignore |
                        &Scope::Preprocessor => None,
                    }
                }
                &Arg::Param { ref scope, ref flag, ref value } => {
                    match scope {
                        &Scope::Compiler | &Scope::Shared => Some("/".to_string() + &flag + &value),
                        &Scope::Preprocessor if task.shared.output_precompiled.is_some() => {
                            Some("/".to_string() + &flag + &value)
                        }
                        &Scope::Ignore |
                        &Scope::Preprocessor => None,
                    }
                }
                &Arg::Input { .. } => None,
                &Arg::Output { .. } => None,
            }
        });
        args.push("/nologo".to_string());
        args.push("/T".to_string() + &task.language);
        if task.shared.output_precompiled.is_some() {
            args.push("/Yc".to_string());
        }
        let output_object = try!(get_output_object(input_source, &task.output_object));
        Ok(CompileStep::new(task, Some(output_object), preprocessed, args, true))
    }

    fn compile_step(&self, state: &SharedState, task: CompileStep) -> Result<OutputInfo, Error> {
        // Input file path.
        let input_temp = TempFile::new_in(self.temp_dir.path(), ".i");
        try!(File::create(input_temp.path()).and_then(|mut s| task.preprocessed.copy(&mut s)));
        // Output file path
        let output_object = task.output_object.expect("Visual Studio don't support compilation to stdout.");
        // Run compiler.
        let mut command = Command::new(&self.path);
        command.env_clear()
            .current_dir(self.temp_dir.path())
            .arg("/c")
            .args(&task.args)
            .arg(input_temp.path().to_str().unwrap())
            .arg(&join_flag("/Fo", &output_object));
        // Copy required environment variables.
        // todo: #15 Need to make correct PATH variable for cl.exe manually
        for (name, value) in vec!["SystemDrive", "SystemRoot", "TEMP", "TMP", "PATH"]
            .iter()
            .filter_map(|name| env::var(name).ok().map(|value| (name, value))) {
            command.env(name, value);
        }
        // Output files.
        match &task.output_precompiled {
            &Some(ref path) => {
                assert!(path.is_absolute());
                command.arg(join_flag("/Fp", path));
            }
            &None => {}
        }
        // Save input file name for output filter.
        let temp_file = input_temp.path()
            .file_name()
            .and_then(|o| o.to_str())
            .map(|o| o.as_bytes())
            .unwrap_or(b"");
        // Use precompiled header
        match &task.input_precompiled {
            &Some(ref path) => {
                assert!(path.is_absolute());
                command.arg("/Yu");
                command.arg(join_flag("/Fp", path));
            }
            &None => {}
        }
        // Execute.
        state.wrap_slow(|| {
            command.output().map(|o| {
                OutputInfo {
                    status: o.status.code(),
                    stdout: prepare_output(temp_file, o.stdout.clone(), o.status.code() == Some(0)),
                    stderr: o.stderr,
                }
            })
        })
    }

    // Compile preprocessed file.
    fn compile_memory(&self, state: &SharedState, mut task: CompileStep) -> Result<(OutputInfo, Vec<u8>), Error> {
        let output_temp = TempFile::new_in(self.temp_dir.path(), ".o");
        task.output_object = Some(output_temp.path().to_path_buf());
        self.compile_step(state, task)
            .and_then(|output| {
                File::open(&output_temp.path()).and_then(|mut f| {
                    let mut buffer = Vec::new();
                    f.read_to_end(&mut buffer).map(|_| (output, buffer))
                })
            })
    }
}

fn get_output_object(input_source: &Path, output_object: &Path) -> Result<PathBuf, Error> {
    match output_object.is_dir() {
        true => {
            input_source.file_name()
                .map(|name| output_object.join(name).with_extension("obj"))
                .ok_or_else(|| {
                    Error::new(ErrorKind::InvalidInput,
                               format!("Input file path does not contains file name: {}",
                                       input_source.to_string_lossy()))
                })
        }
        false => Ok(output_object.to_path_buf()),
    }
}

#[cfg(unix)]
fn vs_identifier(_: &Path) -> Option<String> {
    None
}

#[cfg(windows)]
fn vs_identifier(path: &Path) -> Option<String> {
    // extern crate winapi;
    extern crate kernel32;
    extern crate version;

    use winapi::*;
    use std::convert::Into;
    use std::ffi::OsStr;
    use std::ptr;
    use std::slice;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct LANGANDCODEPAGE {
        language: WORD,
        codepage: WORD,
    };

    fn utf16<'a, T: Into<&'a OsStr>>(value: T) -> Vec<u16> {
        value.into().encode_wide().chain(Some(0).into_iter()).collect()
    };

    let path_raw = utf16(path.as_os_str());
    // Get version info size
    let size = unsafe { version::GetFileVersionInfoSizeW(path_raw.as_ptr(), ptr::null_mut()) };
    if size == 0 {
        return None;
    }
    // Load version info
    let mut data: Vec<u8> = Vec::with_capacity(size as usize);
    unsafe {
        data.set_len(size as usize);
        if version::GetFileVersionInfoW(path_raw.as_ptr(), 0, size, data.as_mut_ptr() as *mut c_void) == 0 {
            return None;
        }
    }
    // Read translation
    let translation_key = unsafe {
        let mut value_size: DWORD = 0;
        let mut value_data: LPVOID = ptr::null_mut();
        if version::VerQueryValueW(data.as_ptr() as LPCVOID,
                                   utf16(OsStr::new("\\VarFileInfo\\Translation")).as_ptr(),
                                   &mut value_data,
                                   &mut value_size) == 0 {
            return None;
        }
        let codepage = value_data as *const LANGANDCODEPAGE;
        format!("\\StringFileInfo\\{:04X}{:04X}",
                (*codepage).language,
                (*codepage).codepage)
    };
    // Read product version
    let product_version = unsafe {
        let mut value_size: DWORD = 0;
        let mut value_data: LPVOID = ptr::null_mut();
        if version::VerQueryValueW(data.as_ptr() as LPCVOID,
                                   utf16(OsStr::new(&(translation_key + "\\ProductVersion"))).as_ptr(),
                                   &mut value_data,
                                   &mut value_size) == 0 {
            return None;
        }
        if value_size == 0 {
            return None;
        }
        String::from_utf16_lossy(slice::from_raw_parts(value_data as *mut u16, (value_size - 1) as usize))
    };
    let executable_id = match read_executable_id(path) {
        Ok(id) => id,
        Err(e) => {
            warn!("{}", e);
            return None;
        }
    };
    Some(format!("cl {} {}", &product_version, executable_id))
}

#[cfg(windows)]
fn read_executable_id(path: &Path) -> Result<String, Error> {
    use byteorder::{LittleEndian, ReadBytesExt};
    use std::io::{ErrorKind, Seek, SeekFrom};

    let mut header: Vec<u8> = Vec::with_capacity(0x54);

    let mut file = try!(File::open(path));
    // Read MZ header
    header.resize(0x40, 0);
    try!(file.read_exact(&mut header[..]));
    // Check MZ header signature
    if header[0..2] != [0x4D, 0x5A] {
        return Err(Error::new(ErrorKind::InvalidData,
                              "Unexpected file type (MZ header signature not found)"));
    }
    // Read PE header offset
    let pe_offset = try!(Cursor::new(&header[0x3C..0x40]).read_u32::<LittleEndian>()) as u64;
    // Read PE header
    try!(file.seek(SeekFrom::Start(pe_offset)));
    header.resize(0x54, 0);
    try!(file.read_exact(&mut header[..]));
    // Check PE header signature
    if header[0..4] != [0x50, 0x45, 0x00, 0x00] {
        return Err(Error::new(ErrorKind::InvalidData,
                              "Unexpected file type (PE header signature not found)"));
    }
    let pe_time_date_stamp = try!(Cursor::new(&header[0x08..0x0C]).read_u32::<LittleEndian>());
    let pe_size_of_image = try!(Cursor::new(&header[0x50..0x54]).read_u32::<LittleEndian>());
    // Read PE header information
    Ok(format!("{:X}{:x}", pe_time_date_stamp, pe_size_of_image))
}

fn prepare_output(line: &[u8], mut buffer: Vec<u8>, success: bool) -> Vec<u8> {
    // Remove strage file name from output
    let mut begin = match (line.len() < buffer.len()) && buffer.starts_with(line) && is_eol(buffer[line.len()]) {
        true => line.len(),
        false => 0,
    };
    while begin < buffer.len() && is_eol(buffer[begin]) {
        begin += 1;
    }
    buffer = buffer.split_off(begin);
    if success {
        // Remove some redundant lines
        lazy_static! {
			static ref RE: Regex = Regex::new(r"(?m)^\S+[^:]*\(\d+\) : warning C4628: .*$\n?").unwrap();
		}
        buffer = RE.replace_all(&buffer, NoExpand(b""))
    }
    buffer
}

fn is_eol(c: u8) -> bool {
    match c {
        b'\r' | b'\n' => true,
        _ => false,
    }
}

fn join_flag(flag: &str, path: &Path) -> String {
    flag.to_string() + &path.to_str().unwrap()
}


#[cfg(test)]
mod test {
    use std::io::Write;

    fn check_prepare_output(original: &str, expected: &str, line: &str, success: bool) {
        let mut stream: Vec<u8> = Vec::new();
        stream.write(&original.as_bytes()[..]).unwrap();

        let result = super::prepare_output(line.as_bytes(), stream, success);
        assert_eq!(String::from_utf8_lossy(&result), expected);
    }

    #[test]
    fn test_prepare_output_simple() {
        check_prepare_output(r#"BLABLABLA
foo.c : warning C4411: foo bar
"#,
                             r#"foo.c : warning C4411: foo bar
"#,
                             "BLABLABLA",
                             true);
    }

    #[test]
    fn test_prepare_output_c4628_remove() {
        check_prepare_output(r#"BLABLABLA
foo.c(41) : warning C4411: foo bar
foo.c(42) : warning C4628: foo bar
foo.c(43) : warning C4433: foo bar
"#,
                             r#"foo.c(41) : warning C4411: foo bar
foo.c(43) : warning C4433: foo bar
"#,
                             "BLABLABLA",
                             true);
    }

    #[test]
    fn test_prepare_output_c4628_keep() {
        check_prepare_output(r#"BLABLABLA
foo.c(41) : warning C4411: foo bar
foo.c(42) : warning C4628: foo bar
foo.c(43) : warning C4433: foo bar
"#,
                             r#"foo.c(41) : warning C4411: foo bar
foo.c(42) : warning C4628: foo bar
foo.c(43) : warning C4433: foo bar
"#,
                             "BLABLABLA",
                             false);
    }
}
