use blu_runtime::{
    Value, Vm,
    bytecode::{LoadLimits, load},
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const PINNED_REVISION: &str = "f8ca77acdcb50241e3da21af663f8ef97b4b5ce4";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("blu-conformance: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse(env::args_os().skip(1))?;
    verify_checkout(&args.source)?;
    verify_executable(&args.upstream)?;
    let compiler = args
        .upstream
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(executable_name("luau-compile"));
    verify_executable(&compiler)?;

    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let return_source = temporary.path().join("return.luau");
    fs::write(&return_source, "return 6 * 7\n").map_err(|error| error.to_string())?;
    let bytecode = Command::new(&compiler)
        .arg("--binary")
        .arg(&return_source)
        .output()
        .map_err(|error| format!("failed to execute {}: {error}", compiler.display()))?;
    ensure_success(&compiler, &bytecode)?;
    let chunk = load(&bytecode.stdout, LoadLimits::default())
        .map_err(|error| format!("Blu rejected upstream bytecode: {error}"))?;
    let blu_result = Vm::default()
        .execute(&chunk)
        .map_err(|error| format!("Blu failed upstream bytecode: {error}"))?;
    if blu_result != [Value::Number(42.0)] {
        return Err(format!("Blu returned {blu_result:?}, expected Number(42)"));
    }

    let print_source = temporary.path().join("print.luau");
    fs::write(&print_source, "print(6 * 7)\n").map_err(|error| error.to_string())?;
    let reference = Command::new(&args.upstream)
        .arg(&print_source)
        .output()
        .map_err(|error| {
            format!(
                "failed to execute reference {}: {error}",
                args.upstream.display()
            )
        })?;
    ensure_success(&args.upstream, &reference)?;
    if String::from_utf8_lossy(&reference.stdout).trim() != "42" {
        return Err(format!(
            "reference returned unexpected output {:?}",
            String::from_utf8_lossy(&reference.stdout)
        ));
    }

    println!("pinned Luau revision: {PINNED_REVISION}");
    println!("bytecode version: {}", chunk.version);
    println!("scalar differential smoke: pass (42)");
    Ok(())
}

fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

fn verify_checkout(path: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
    ensure_success(Path::new("git"), &output)?;
    let actual = String::from_utf8_lossy(&output.stdout);
    if actual.trim() != PINNED_REVISION {
        return Err(format!(
            "upstream checkout is {}, expected {PINNED_REVISION}",
            actual.trim()
        ));
    }
    Ok(())
}

fn verify_executable(path: &Path) -> Result<(), String> {
    let output = Command::new(path)
        .arg("--help")
        .output()
        .map_err(|error| format!("failed to execute {}: {error}", path.display()))?;
    ensure_success(path, &output)
}

fn ensure_success(path: &Path, output: &std::process::Output) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} failed with {}: {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Args {
    upstream: PathBuf,
    source: PathBuf,
}

impl Args {
    fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut upstream = None;
        let mut source = None;
        let mut args = args;
        while let Some(argument) = args.next() {
            match argument.to_str() {
                Some("--upstream") => {
                    upstream = Some(args.next().ok_or("--upstream requires a path")?.into());
                }
                Some("--source") => {
                    source = Some(args.next().ok_or("--source requires a path")?.into());
                }
                _ => {
                    return Err(format!(
                        "usage: blu-conformance --upstream <luau> --source <luau-checkout>; \
                         unexpected argument {}",
                        argument.to_string_lossy()
                    ));
                }
            }
        }
        Ok(Self {
            upstream: upstream.ok_or("missing --upstream")?,
            source: source.ok_or("missing --source")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_reference_runtime_and_source() {
        let args = Args::parse(
            ["--upstream", "/tmp/build/luau", "--source", "/tmp/source"]
                .into_iter()
                .map(std::ffi::OsString::from),
        )
        .unwrap();
        assert_eq!(
            args,
            Args {
                upstream: "/tmp/build/luau".into(),
                source: "/tmp/source".into()
            }
        );
        assert!(Args::parse(std::iter::empty()).is_err());
    }
}
