// SPDX-License-Identifier: GPL-3.0-or-later
//! Isolated CAD inspection jobs with private request/result files and explicit cancellation ownership.

use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::cad::scene::{Request, Scene};

static NEXT_JOB: AtomicU64 = AtomicU64::new(0);

/// The result file of a view job: the scene, or why it failed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Completed { scene: Box<Scene> },
    Failed { message: String },
}

/// Worker entry point: always emit a structured SDK/import failure when possible.
pub fn execute(request_path: &Path, result_path: &Path) -> Result<()> {
    respond(request_path, result_path, crate::cad::gmsh::run)
}

/// The resident view server (`cad2bem --view-server`): it reads `REQUEST<TAB>RESULT` lines on stdin and
/// answers each as [`execute`] does, keeping one Gmsh session (and its caches) between requests, so a view
/// after a settings change does not start a process and Gmsh again. It ends at the end of its input.
pub fn serve() -> Result<()> {
    use std::io::BufRead;
    let mut session: Option<(Option<PathBuf>, crate::cad::session::Session)> = None;
    let mut held = None;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let (request, result) = line
            .split_once('\t')
            .context("View server input: REQUEST<TAB>RESULT per line")?;
        respond(Path::new(request), Path::new(result), |request| {
            // A session is for one SDK library; another one is refused (the client starts a new server).
            match &session {
                Some((library, _)) => ensure!(
                    *library == request.library,
                    "The view server runs another Gmsh SDK"
                ),
                None => {
                    let opened = crate::cad::session::Session::open(request.library.as_deref())?;
                    session = Some((request.library.clone(), opened));
                }
            }
            let (_, gmsh) = session.as_ref().expect("opened");
            crate::cad::gmsh::run_in(gmsh, request, &mut held)
        })?;
    }
    Ok(())
}

/// Read a request, run it and write the response file (atomically, by renaming a finished file).
fn respond(
    request_path: &Path,
    result_path: &Path,
    run: impl FnOnce(&Request) -> Result<Scene>,
) -> Result<()> {
    ensure!(!result_path.exists(), "Worker result path already exists");
    let response = (|| -> Result<Scene> {
        ensure!(
            fs::metadata(request_path)?.len() < 1024 * 1024,
            "Job request is too large"
        );
        let request: Request = serde_json::from_reader(BufReader::new(File::open(request_path)?))?;
        run(&request)
    })();
    let response = match response {
        Ok(scene) => Response::Completed {
            scene: Box::new(scene),
        },
        Err(error) => Response::Failed {
            message: format!("{error:#}"),
        },
    };
    let temporary = result_path.with_extension("pending");
    let mut writer = BufWriter::new(File::create(&temporary)?);
    serde_json::to_writer(&mut writer, &response)?;
    writer.flush()?;
    drop(writer);
    fs::rename(temporary, result_path)?;
    Ok(())
}

/// The resident view server of this process, started on the first view job and restarted when it stops,
/// when a job is cancelled (it is killed) or when another SDK library is asked for.
struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    program: PathBuf,
    library: Option<PathBuf>,
    stderr: PathBuf,
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

impl Server {
    fn start(program: &Path, library: Option<PathBuf>) -> Result<Self> {
        let stderr = std::env::temp_dir().join(format!(
            "cad2bem-{}-view-server-{}.txt",
            std::process::id(),
            NEXT_JOB.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new(program);
        command
            .arg("--view-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(File::create(&stderr)?);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().context("View server has no input")?;
        Ok(Self {
            child,
            stdin,
            program: program.to_owned(),
            library,
            stderr,
        })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.stderr);
    }
}

fn stderr_text(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .chars()
        .take(8000)
        .collect()
}

enum Runner {
    /// A worker process of its own (`--worker`), which exits after the job.
    Process(Child),
    /// The resident view server.
    Resident,
}

/// A view job running in a worker process or in the resident view server.
pub struct Job {
    runner: Runner,
    directory: PathBuf,
    done: bool,
    pub started: Instant,
}

impl Job {
    /// Run a view job in the resident view server.
    pub fn start(request: &Request) -> Result<Self> {
        Self::start_resident(request, &std::env::current_exe()?)
    }

    fn directory() -> Result<PathBuf> {
        let directory = std::env::temp_dir().join(format!(
            "cad2bem-{}-{}",
            std::process::id(),
            NEXT_JOB.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).context("Cannot create a private job directory")?;
        Ok(directory)
    }

    /// Run a view job in the resident view server of `program` (started or replaced as needed).
    pub fn start_resident(request: &Request, program: &Path) -> Result<Self> {
        request.validate()?;
        let directory = Self::directory()?;
        let send = (|| -> Result<()> {
            let request_path = directory.join("request.json");
            fs::write(&request_path, serde_json::to_vec(request)?)?;
            let text = |p: &Path| -> Result<String> {
                Ok(p.to_str()
                    .context("Temporary path is not valid Unicode")?
                    .to_owned())
            };
            let line = format!(
                "{}\t{}\n",
                text(&request_path)?,
                text(&directory.join("result.json"))?
            );
            let mut server = SERVER.lock().unwrap_or_else(|e| e.into_inner());
            let usable = server.as_mut().is_some_and(|s| {
                s.running() && s.program == program && s.library == request.library
            });
            if !usable {
                if let Some(old) = server.take() {
                    old.kill();
                }
                *server = Some(Server::start(program, request.library.clone())?);
            }
            let s = server.as_mut().expect("started");
            if let Err(error) = s
                .stdin
                .write_all(line.as_bytes())
                .and_then(|()| s.stdin.flush())
            {
                if let Some(old) = server.take() {
                    old.kill();
                }
                return Err(error.into());
            }
            Ok(())
        })();
        match send {
            Ok(()) => Ok(Self {
                runner: Runner::Resident,
                directory,
                done: false,
                started: Instant::now(),
            }),
            Err(error) => {
                cleanup(&directory);
                Err(error)
            }
        }
    }

    /// Run a view job in a worker process of its own (integration tests launch the actual worker binary).
    pub fn start_with_program(request: &Request, program: &Path) -> Result<Self> {
        request.validate()?;
        let directory = Self::directory()?;
        let spawn = (|| -> Result<Child> {
            let request_path = directory.join("request.json");
            fs::write(&request_path, serde_json::to_vec(request)?)?;
            let mut command = Command::new(program);
            command.args([
                std::ffi::OsStr::new("--worker"),
                request_path.as_os_str(),
                directory.join("result.json").as_os_str(),
            ]);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(directory.join("stderr.txt"))?);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: the worker has no interactive UI.
            }
            Ok(command.spawn()?)
        })();
        match spawn {
            Ok(child) => Ok(Self {
                runner: Runner::Process(child),
                directory,
                done: false,
                started: Instant::now(),
            }),
            Err(error) => {
                cleanup(&directory);
                Err(error)
            }
        }
    }

    /// Poll without blocking the UI. A result is delivered once its file is complete (a worker process has
    /// also exited).
    pub fn poll(&mut self) -> Option<Result<Scene>> {
        let result = self.directory.join("result.json");
        let outcome = match &mut self.runner {
            Runner::Process(child) => match child.try_wait() {
                Ok(None) => return None,
                Err(error) => Err(error.into()),
                Ok(Some(status)) => {
                    let stderr = stderr_text(&self.directory.join("stderr.txt"));
                    if !status.success() {
                        Err(anyhow::anyhow!("Worker exited with {status}: {stderr}"))
                    } else if !result.is_file() {
                        Err(anyhow::anyhow!("Worker exited without a result: {stderr}"))
                    } else {
                        read_result(&result)
                    }
                }
            },
            Runner::Resident => {
                if result.is_file() {
                    read_result(&result)
                } else {
                    let mut server = SERVER.lock().unwrap_or_else(|e| e.into_inner());
                    match server.as_mut().map(|s| s.running()) {
                        Some(true) => return None,
                        Some(false) => {
                            let old = server.take().expect("checked");
                            let stderr = stderr_text(&old.stderr);
                            old.kill();
                            Err(anyhow::anyhow!("The view server stopped: {stderr}"))
                        }
                        None => Err(anyhow::anyhow!("The view server stopped")),
                    }
                }
            }
        };
        self.done = true;
        Some(outcome)
    }

    /// Stop the job: its worker process, or the resident server when the job is still running there (the
    /// next view job starts a new one).
    pub fn cancel(&mut self) {
        match &mut self.runner {
            Runner::Process(child) => {
                if child.try_wait().ok().flatten().is_none() {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
            Runner::Resident => {
                if !self.done && !self.directory.join("result.json").is_file() {
                    let mut server = SERVER.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(s) = server.take() {
                        s.kill();
                    }
                }
            }
        }
        self.done = true;
    }
}

fn read_result(result: &Path) -> Result<Scene> {
    ensure!(
        fs::metadata(result)?.len() <= 512 * 1024 * 1024,
        "Worker scene exceeds the 512 MiB preview limit"
    );
    match serde_json::from_reader(BufReader::new(File::open(result)?))? {
        Response::Completed { scene } => {
            scene.validate()?;
            Ok(*scene)
        }
        Response::Failed { message } => Err(anyhow::anyhow!(message)),
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        // This directory was exclusively created by this job, never derived from a CAD/user path.
        cleanup(&self.directory);
    }
}

fn cleanup(directory: &Path) {
    // Verify the resolved absolute target before recursively deleting any job directory.
    if let (Ok(root), Ok(target)) = (
        std::env::temp_dir().canonicalize(),
        directory.canonicalize(),
    ) {
        let prefix = format!("cad2bem-{}-", std::process::id());
        if target.parent() == Some(root.as_path())
            && target
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        {
            let _ = fs::remove_dir_all(target);
        }
    }
}
