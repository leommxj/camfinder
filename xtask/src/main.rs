use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const PACKAGE: &str = "camfinder-cardputer-zero";
const DEFAULT_TARGET: &str = "aarch64-unknown-linux-gnu";

struct Options {
    target: String,
    binary: Option<PathBuf>,
    screenshot: Option<PathBuf>,
    output: Option<PathBuf>,
    no_build: bool,
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("cardputer-zero") => package(parse(args.collect())?),
        None | Some("--help" | "-h") => {
            help();
            Ok(())
        }
        Some(command) => bail!("unknown command {command}; use --help"),
    }
}

fn help() {
    println!("cargo xtask cardputer-zero [OPTIONS]\n\nBuild an ARM64 CardputerZero Debian package and review metadata.\n\n  --target TRIPLE      Build target (default: {DEFAULT_TARGET})\n  --binary PATH        Package an existing ARM64 ELF without building\n  --no-build           Use the existing release binary for --target\n  --screenshot PATH    Real 320x170 application screenshot\n  --output DIR         Output directory (default: Cargo target directory/dist)\n  -h, --help           Show help\n\nUse CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER and target-specific\nRUSTFLAGS for cross toolchains. Native ARM64 builds need no linker override.");
}

fn parse(args: Vec<String>) -> Result<Options> {
    let mut options = Options {
        target: DEFAULT_TARGET.into(),
        binary: None,
        screenshot: None,
        output: None,
        no_build: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => options.target = args.next().context("--target requires a value")?,
            "--binary" => {
                options.binary = Some(args.next().context("--binary requires a path")?.into())
            }
            "--screenshot" => {
                options.screenshot =
                    Some(args.next().context("--screenshot requires a path")?.into())
            }
            "--output" => {
                options.output = Some(args.next().context("--output requires a directory")?.into())
            }
            "--no-build" => options.no_build = true,
            "--help" | "-h" => {
                help();
                std::process::exit(0);
            }
            _ => bail!("unknown option {arg}; use --help"),
        }
    }
    Ok(options)
}

fn package(options: Options) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let metadata: Value = serde_json::from_slice(&run(Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1"]))?)?;
    let app = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == PACKAGE)
        .context("missing CardputerZero package")?;
    let version = app["version"].as_str().context("missing package version")?;
    let target_dir = PathBuf::from(metadata["target_directory"].as_str().unwrap());
    if options.binary.is_none() && !options.no_build {
        run(Command::new("cargo").current_dir(root).args([
            "build",
            "--locked",
            "--release",
            "--package",
            PACKAGE,
            "--target",
            &options.target,
        ]))?;
    }
    let binary = options.binary.unwrap_or_else(|| {
        target_dir
            .join(&options.target)
            .join("release")
            .join(PACKAGE)
    });
    validate_elf(&binary)?;
    let packaging = root.join("apps/cardputer-zero/packaging");
    let screenshot = options
        .screenshot
        .unwrap_or_else(|| packaging.join("assets/screenshot.png"));
    validate_png(&screenshot, 320, 170)?;
    validate_png(&packaging.join("assets/icon.png"), 256, 256)?;
    let output = options.output.unwrap_or_else(|| target_dir.join("dist"));
    fs::create_dir_all(&output)?;
    let review = output.join(format!("{PACKAGE}_{version}-1_arm64"));
    if review.exists() {
        fs::remove_dir_all(&review)?;
    }
    fs::create_dir_all(review.join("assets"))?;
    let mut builder: Value =
        serde_json::from_slice(&fs::read(packaging.join("app-builder.json"))?)?;
    builder["version"] = Value::String(version.into());
    validate_builder(&builder)?;
    fs::write(
        review.join("app-builder.json"),
        serde_json::to_vec_pretty(&builder)?,
    )?;
    fs::copy(
        packaging.join("assets/icon.png"),
        review.join("assets/icon.png"),
    )?;
    fs::copy(screenshot, review.join("assets/screenshot.png"))?;
    let staging =
        Staging(env::temp_dir().join(format!("camfinder-package-{}", std::process::id())));
    fs::create_dir(&staging.0)?;
    stage(root, &packaging, &review, &staging.0, &binary, version)?;
    directory_modes(&staging.0)?;
    let deb = output.join(format!("{PACKAGE}_{version}-1_arm64.deb"));
    run(Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&staging.0)
        .arg(&deb))?;
    fs::copy(staging.0.join("DEBIAN/control"), review.join("control"))?;
    let contents = run(Command::new("dpkg-deb").arg("--contents").arg(&deb))?;
    fs::write(review.join("package-files.txt"), contents)?;
    println!("Package: {}\nReview: {}", deb.display(), review.display());
    Ok(())
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    let output = command
        .output()
        .with_context(|| format!("failed to run {command:?}"))?;
    ensure!(
        output.status.success(),
        "command failed: {command:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

fn validate_elf(path: &Path) -> Result<()> {
    let header = fs::read(path)
        .with_context(|| format!("failed to read ARM64 binary {}", path.display()))?;
    ensure!(
        header.len() >= 20
            && &header[..4] == b"\x7fELF"
            && header[4] == 2
            && header[5] == 1
            && u16::from_le_bytes([header[18], header[19]]) == 183,
        "{} must be a little-endian ARM64 ELF64 binary",
        path.display()
    );
    Ok(())
}

fn validate_png(path: &Path, width: u32, height: u32) -> Result<()> {
    let decoder = png::Decoder::new(fs::File::open(path).with_context(|| {
        format!(
            "missing PNG {}; generate it with the application --demo --screenshot",
            path.display()
        )
    })?);
    let mut reader = decoder.read_info()?;
    ensure!(
        reader.info().width == width && reader.info().height == height,
        "{} must be a {width}x{height} PNG",
        path.display()
    );
    let mut pixels = vec![0; reader.output_buffer_size()];
    reader.next_frame(&mut pixels)?;
    Ok(())
}

fn validate_builder(builder: &Value) -> Result<()> {
    ensure!(
        builder["package_name"] == PACKAGE,
        "app-builder package_name mismatch"
    );
    ensure!(
        builder["app_name"].is_string() && builder["store"]["summary"].is_string(),
        "app_name and store.summary are required"
    );
    let store = &builder["store"];
    ensure!(
        store["author"]["display_name"] == "leommxj" && store["license"] == "WTFPL",
        "author or license mismatch"
    );
    ensure!(
        store["icon"] == "assets/icon.png"
            && store["screenshots"]
                .as_array()
                .is_some_and(|images| images.len() == 1 && images[0] == "assets/screenshot.png"),
        "unexpected store asset paths"
    );
    let categories = store["categories"]
        .as_array()
        .context("store.categories is required")?;
    ensure!(
        (1..=2).contains(&categories.len())
            && categories.iter().all(|category| matches!(
                category.as_str(),
                Some("Radio & Comms" | "Hardware & IoT")
            )),
        "invalid store categories"
    );
    let code = store["share_code"]
        .as_str()
        .context("store.share_code is required")?;
    ensure!(
        code.len() == 4
            && code
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "share_code must contain four uppercase letters or digits"
    );
    for permission in [
        "camera",
        "microphone",
        "imu",
        "network",
        "additional_hardware",
        "background_service",
        "external_display",
    ] {
        ensure!(
            store["permissions"][permission].as_bool() == Some(permission == "additional_hardware"),
            "invalid permission {permission}"
        );
    }
    Ok(())
}

fn stage(
    root: &Path,
    packaging: &Path,
    review: &Path,
    staging: &Path,
    binary: &Path,
    version: &str,
) -> Result<()> {
    let launch = staging.join("usr/share/APPLaunch");
    copy(binary, &launch.join("bin").join(PACKAGE), 0o755)?;
    copy(
        &packaging.join("camfinder.desktop"),
        &launch.join("applications/camfinder.desktop"),
        0o644,
    )?;
    copy(
        &packaging.join("assets/icon.png"),
        &launch.join("share/images/camfinder.png"),
        0o644,
    )?;
    copy(
        &packaging.join("60-camfinder-airspy.rules"),
        &staging.join("usr/lib/udev/rules.d/60-camfinder-airspy.rules"),
        0o644,
    )?;
    let data = staging.join("usr/share/camfinder");
    copy(
        &root.join("camfinder.default.toml"),
        &data.join("camfinder.default.toml"),
        0o644,
    )?;
    for entry in fs::read_dir(root.join("profiles"))? {
        let entry = entry?;
        copy(
            &entry.path(),
            &data.join("profiles").join(entry.file_name()),
            0o644,
        )?;
    }
    let doc = staging.join("usr/share/doc").join(PACKAGE);
    copy(&root.join("LICENSE"), &doc.join("LICENSE"), 0o644)?;
    copy(
        &root.join("vendor/rs-spy/LICENSE"),
        &doc.join("rs-spy-LICENSE"),
        0o644,
    )?;
    let store = staging.join("usr/share").join(PACKAGE);
    copy(
        &review.join("app-builder.json"),
        &store.join("app-builder.json"),
        0o644,
    )?;
    copy(
        &review.join("assets/icon.png"),
        &store.join("assets/icon.png"),
        0o644,
    )?;
    copy(
        &review.join("assets/screenshot.png"),
        &store.join("assets/screenshot.png"),
        0o644,
    )?;
    let dynamic = String::from_utf8(run(Command::new("readelf")
        .args(["--wide", "--dynamic"])
        .arg(binary))?)?;
    let versions = String::from_utf8(run(Command::new("readelf")
        .args(["--wide", "--version-info"])
        .arg(binary))?)?;
    fs::write(review.join("elf-dynamic.txt"), &dynamic)?;
    fs::write(review.join("elf-versions.txt"), &versions)?;
    let glibc = versions
        .split("GLIBC_")
        .skip(1)
        .filter_map(|tail| {
            let version: String = tail
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parts = version
                .split('.')
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()?;
            Some((parts, version))
        })
        .max_by(|left, right| left.0.cmp(&right.0))
        .context("binary has no GLIBC symbol versions")?
        .1;
    let dependencies = [
        ("libSDL2-2.0.so.0", "libsdl2-2.0-0 (>= 2.0.18)"),
        ("libSDL2_ttf-2.0.so.0", "libsdl2-ttf-2.0-0 (>= 2.0.15)"),
        ("libSoapySDR.so.0.8", "libsoapysdr0.8 (>= 0.8.0)"),
        ("libgcc_s.so.1", "libgcc-s1"),
    ];
    for line in dynamic.lines().filter(|line| line.contains("(NEEDED)")) {
        let soname = line
            .split('[')
            .nth(1)
            .and_then(|value| value.split(']').next())
            .context("invalid ELF dependency")?;
        ensure!(
            [
                "libc.so.6",
                "libm.so.6",
                "libdl.so.2",
                "libpthread.so.0",
                "librt.so.1",
                "ld-linux-aarch64.so.1"
            ]
            .contains(&soname)
                || dependencies.iter().any(|(name, _)| *name == soname),
            "no Debian dependency mapping for {soname}"
        );
    }
    let mut depends = vec![format!("libc6 (>= {glibc})")];
    for (soname, package) in dependencies {
        if dynamic.contains(&format!("[{soname}]")) {
            depends.push(package.into());
        }
    }
    ensure!(
        dynamic.contains("[libSDL2-2.0.so.0]") && dynamic.contains("[libSDL2_ttf-2.0.so.0]"),
        "application must dynamically link SDL2 and SDL2_ttf"
    );
    depends.push("fonts-dejavu-core".into());
    let control = staging.join("DEBIAN");
    fs::create_dir_all(&control)?;
    let size = directory_size(staging)? / 1024 + 1;
    fs::write(control.join("control"), format!("Package: {PACKAGE}\nVersion: {version}-1\nArchitecture: arm64\nMaintainer: leommxj\nSection: APPLaunch\nPriority: optional\nInstalled-Size: {size}\nDepends: {}\nDescription: Experimental SDR-assisted camera leakage detector for Cardputer Zero\n Scan crystal-clock harmonics for weak electromagnetic leakage and track\n persistent candidates on the built-in display.\n Practical detection relies heavily on an external low-noise amplifier\n (LNA). The original tests used a 20 dB LNA; Airspy Mini and USRP B210 alone\n did not capture the leakage effectively.\n Receiver support depends on the backends enabled at build time.\n", depends.join(", ")))?;
    copy(
        &packaging.join("postinst"),
        &control.join("postinst"),
        0o755,
    )?;
    copy(&packaging.join("postrm"), &control.join("postrm"), 0o755)?;
    Ok(())
}

fn copy(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::copy(source, destination)
        .with_context(|| format!("failed to copy {}", source.display()))?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut size = 0;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        size += if path.is_dir() {
            directory_size(&path)?
        } else {
            path.metadata()?.len()
        };
    }
    Ok(size)
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn directory_modes(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            directory_modes(&path)?;
        }
    }
    Ok(())
}
