pub use ::compiler::*;

use ::filter::comments::CommentsRemover;
use ::io::memstream::MemStream;
use ::lazy::Lazy;

use regex;
use regex::Regex;

use std::env;
use std::io;
use std::io::{Error, Read};
use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

lazy_static! {
	static ref RE_CLANG: regex::bytes::Regex = regex::bytes::Regex::new(
	r"(?i)^(clang(:?\+\+)?)(-\d+\.\d+)?(?:.exe)?$"
	).unwrap();
}

pub struct ClangCompiler {
    toolchains: ToolchainHolder,
}

impl ClangCompiler {
    pub fn new() -> Self {
        ClangCompiler { toolchains: ToolchainHolder::new() }
    }
}

struct ClangToolchain {
    path: PathBuf,
    identifier: Lazy<Option<String>>,
}

impl ClangToolchain {
    pub fn new(path: PathBuf) -> Self {
        ClangToolchain {
            path: path,
            identifier: Lazy::new(),
        }
    }
}

impl Compiler for ClangCompiler {
    fn resolve_toolchain(&self, command: &CommandInfo) -> Option<Arc<Toolchain>> {
        if command.program
            .file_name()
            .map_or(false, |n| RE_CLANG.is_match(n.to_string_lossy().as_bytes())) {
            command.find_executable()
                .and_then(|path| self.toolchains.resolve(&path, |path| Arc::new(ClangToolchain::new(path))))
        } else {
            None
        }
    }

    fn discovery_toolchains(&self) -> Vec<Arc<Toolchain>> {
        env::var_os("PATH")
            .map_or(Vec::new(), |paths| env::split_paths(&paths).collect())
            .iter()
            .filter(|path| path.is_absolute())
            .filter_map(|path| path.read_dir().ok())
            .flat_map(|read_dir| read_dir)
            .filter_map(|entry| entry.ok())
            .filter(|entry| RE_CLANG.is_match(entry.file_name().to_string_lossy().as_bytes()))
            .map(|entry| -> Arc<Toolchain> { Arc::new(ClangToolchain::new(entry.path())) })
            .collect()
    }
}

impl Toolchain for ClangToolchain {
    fn identifier(&self) -> Option<String> {
        self.identifier.get(|| clang_identifier(&self.path))
    }

    fn create_tasks(&self, command: CommandInfo, args: &[String]) -> Result<Vec<CompilationTask>, String> {
        super::prepare::create_tasks(command, args)
    }

    fn preprocess_step(&self,
                       state: &SharedState,
                       task: &CompilationTask,
                       worker: &Fn(&Path, PreprocessResult) -> Result<(), Error>)
                       -> Result<(), Error> {
        let mut args = Vec::new();
        args.push("-E".to_string());
        args.push("-x".to_string());
        args.push(task.language.clone());
        args.push("-frewrite-includes".to_string());

        // Make parameters list for preprocessing.
        for arg in task.shared.args.iter() {
            match arg {
                &Arg::Flag { ref scope, ref flag } => {
                    match scope {
                        &Scope::Preprocessor |
                        &Scope::Shared => {
                            args.push("-".to_string() + &flag);
                        }
                        &Scope::Ignore | &Scope::Compiler => {}
                    }
                }
                &Arg::Param { ref scope, ref flag, ref value } => {
                    match scope {
                        &Scope::Preprocessor |
                        &Scope::Shared => {
                            args.push("-".to_string() + &flag);
                            args.push(value.clone());
                        }
                        &Scope::Ignore | &Scope::Compiler => {}
                    }
                }
                &Arg::Input { .. } => {}
                &Arg::Output { .. } => {}
            };
        }

        // Add preprocessor paramters.
        assert!(task.input_sources.len() == 1);
        args.push(task.input_sources[0].display().to_string());
        args.push("-o".to_string());
        args.push("-".to_string());

        state.wrap_slow(|| execute(task.shared.command.to_command().args(&args)))
            .and_then(|r| worker(&task.output_object, r))
    }

    // Compile preprocessed file.
    fn compile_prepare_step(&self,
                            task: &CompilationTask,
                            _: &Path,
                            preprocessed: MemStream)
                            -> Result<CompileStep, Error> {
        let mut args = Vec::new();
        args.push("-x".to_string());
        args.push(task.language.clone());
        for arg in task.shared.args.iter() {
            match arg {
                &Arg::Flag { ref scope, ref flag } => {
                    match scope {
                        &Scope::Compiler | &Scope::Shared => {
                            args.push("-".to_string() + &flag);
                        }
                        &Scope::Ignore |
                        &Scope::Preprocessor => {}
                    }
                }
                &Arg::Param { ref scope, ref flag, ref value } => {
                    match scope {
                        &Scope::Compiler | &Scope::Shared => {
                            args.push("-".to_string() + &flag);
                            args.push(value.clone());
                        }
                        &Scope::Ignore |
                        &Scope::Preprocessor => {}
                    }
                }
                &Arg::Input { .. } => {}
                &Arg::Output { .. } => {}
            };
        }
        Ok(CompileStep::new(task,
                            Some(task.output_object.clone()),
                            preprocessed,
                            args,
                            false))
    }

    fn compile_step(&self, state: &SharedState, task: CompileStep) -> Result<OutputInfo, Error> {
        // Run compiler.
        state.wrap_slow(|| {
            Command::new(&self.path)
                .env_clear()
                .arg("-c")
                .args(&task.args)
                .arg("-")
                .arg("-o")
                .arg(task.output_object.as_ref().map_or("-".to_string(), |path| path.display().to_string()))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    try!(task.preprocessed.copy(child.stdin.as_mut().unwrap()));
                    let _ = task.preprocessed;
                    child.wait_with_output()
                })
                .map(|o| OutputInfo::new(o))
        })
    }
}

fn clang_parse_version(base_name: &str, stdout: &str) -> Option<String> {
    lazy_static!{
		static ref RE: Regex = Regex::new(r"^.*clang.*?\((\S+)\).*\nTarget:\s*(\S+)").unwrap();
	}

    RE.captures_iter(&stdout)
        .next()
        .and_then(|cap| {
            Some(format!("{} {} {}",
                         base_name,
                         cap.at(1).unwrap_or(""),
                         cap.at(2).unwrap_or("")))
        })
}

fn clang_identifier(clang: &Path) -> Option<String> {
    clang.file_name()
        .and_then(|file_name| {
            RE_CLANG.captures_iter(file_name.to_string_lossy().as_bytes())
                .next()
                .and_then(|cap| cap.at(1))
                .map(|base_name| String::from_utf8_lossy(base_name).into_owned())
        })
        .and_then(|base_name| {
            Command::new(clang.as_os_str())
                .arg("--version")
                .output()
                .ok()
                .and_then(|output| {
                    match output.status.success() {
                        true => Some(String::from_utf8_lossy(&output.stdout).to_string()),
                        false => None,
                    }
                })
                .and_then(|stdout| clang_parse_version(&base_name, &stdout))
        })
}

fn execute(command: &mut Command) -> Result<PreprocessResult, Error> {
    let mut child = try!(command.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn());
    drop(child.stdin.take());

    fn read_stdout<T: Read>(stream: Option<T>) -> MemStream {
        stream.map_or(Ok(MemStream::new()), |mut stream| {
                let mut ret = MemStream::new();
                io::copy(&mut stream, &mut ret).map(|_| ret)
            })
            .unwrap_or(MemStream::new())
    }

    fn read_stderr<T: Read + Send + 'static>(stream: Option<T>) -> Receiver<Result<Vec<u8>, Error>> {
        let (tx, rx) = channel();
        match stream {
            Some(mut stream) => {
                thread::spawn(move || {
                    let mut ret = Vec::new();
                    let res = stream.read_to_end(&mut ret).map(|_| ret);
                    tx.send(res).unwrap();
                });
            }
            None => tx.send(Ok(Vec::new())).unwrap(),
        }
        rx
    }

    fn bytes(stream: Receiver<Result<Vec<u8>, Error>>) -> Vec<u8> {
        stream.recv().unwrap().unwrap_or(Vec::new())
    }

    let rx_err = read_stderr(child.stderr.take());
    let stdout = read_stdout(child.stdout.take().map(|f| CommentsRemover::new(f)));
    let status = try!(child.wait());
    let stderr = bytes(rx_err);

    if status.success() {
        Ok(PreprocessResult::Success(stdout))
    } else {
        Ok(PreprocessResult::Failed(OutputInfo {
            status: status.code(),
            stdout: Vec::new(),
            stderr: stderr,
        }))
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test_ubuntu_14_04_clang_3_5() {
        assert_eq!(super::clang_parse_version("prefix",
r#"Ubuntu clang version 3.5.0-4ubuntu2~trusty2 (tags/RELEASE_350/final) (based on LLVM 3.5.0)
Target: x86_64-pc-linux-gnu
Thread model: posix
"#), Some("prefix tags/RELEASE_350/final x86_64-pc-linux-gnu".to_string()))
    }

    #[test]
    fn test_ubuntu_14_04_clang_3_6() {
        assert_eq!(super::clang_parse_version("prefix",
r#"Ubuntu clang version 3.6.0-2ubuntu1~trusty1 (tags/RELEASE_360/final) (based on LLVM 3.6.0)
Target: x86_64-pc-linux-gnu
Thread model: posix
"#), Some("prefix tags/RELEASE_360/final x86_64-pc-linux-gnu".to_string()))
    }

    #[test]
    fn test_ubuntu_16_04_clang_3_5() {
        assert_eq!(super::clang_parse_version("prefix",
r#"Ubuntu clang version 3.5.2-3ubuntu1 (tags/RELEASE_352/final) (based on LLVM 3.5.2)
Target: x86_64-pc-linux-gnu
Thread model: posix
"#), Some("prefix tags/RELEASE_352/final x86_64-pc-linux-gnu".to_string()))
    }

    #[test]
    fn test_ubuntu_16_04_clang_3_6() {
        assert_eq!(super::clang_parse_version("prefix",
r#"Ubuntu clang version 3.6.2-3ubuntu2 (tags/RELEASE_362/final) (based on LLVM 3.6.2)
Target: x86_64-pc-linux-gnu
Thread model: posix
"#), Some("prefix tags/RELEASE_362/final x86_64-pc-linux-gnu".to_string()))
    }

    #[test]
    fn test_ubuntu_16_04_clang_3_7() {
        assert_eq!(super::clang_parse_version("prefix",
r#"Ubuntu clang version 3.7.1-2ubuntu2 (tags/RELEASE_371/final) (based on LLVM 3.7.1)
Target: x86_64-pc-linux-gnu
Thread model: posix
"#), Some("prefix tags/RELEASE_371/final x86_64-pc-linux-gnu".to_string()))
    }

    #[test]
    fn test_ubuntu_16_04_clang_3_8() {
        assert_eq!(super::clang_parse_version("prefix",
                                              r#"clang version 3.8.0-2ubuntu3 (tags/RELEASE_380/final)
Target: x86_64-pc-linux-gnu
Thread model: posix
InstalledDir: /usr/bin
"#),
                   Some("prefix tags/RELEASE_380/final x86_64-pc-linux-gnu".to_string()))
    }
}
