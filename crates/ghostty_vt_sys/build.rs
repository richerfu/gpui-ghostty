use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("ghostty_vt_sys must live under crates/*");

    let ghostty_dir = workspace_root.join("vendor/ghostty");
    println!(
        "cargo:rerun-if-changed={}",
        ghostty_dir.join("build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("include/ghostty_vt.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/build.zig").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/build.zig.zon").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("zig/lib.zig").display()
    );

    if !ghostty_dir.exists() {
        panic!(
            "vendor/ghostty is missing; run `git submodule update --init --recursive` and retry"
        );
    }

    let zig = find_zig(workspace_root);
    let zig_version = Command::new(&zig).arg("version").output().ok();
    if zig_version.is_none() {
        panic!(
            "`zig` is required; run `./scripts/bootstrap-zig.sh` \
to install Zig 0.14.1 into .context/zig/zig"
        );
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let prefix = out_dir.join("zig-out");
    let zig_target = zig_target_from_rust_target();

    let first = run_zig_build(&zig, &manifest_dir, &prefix, zig_target.as_deref());
    if !first.status.success() {
        let stderr = String::from_utf8_lossy(&first.stderr);
        let patched = maybe_patch_ziglyph_build_for_zig_016(stderr.as_ref());
        if patched {
            let retry = run_zig_build(&zig, &manifest_dir, &prefix, zig_target.as_deref());
            if !retry.status.success() {
                print_build_output(&retry);
                panic!("zig build failed after applying ziglyph compatibility patch");
            }
        } else {
            print_build_output(&first);
            panic!("zig build failed");
        }
    }

    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=ghostty_vt");
    println!("cargo:rustc-link-lib=c");
}

fn run_zig_build(
    zig: &std::path::Path,
    manifest_dir: &std::path::Path,
    prefix: &std::path::Path,
    zig_target: Option<&str>,
) -> std::process::Output {
    let mut cmd = Command::new(zig);
    cmd.current_dir(manifest_dir.join("zig"))
        .arg("build")
        .arg("-Doptimize=ReleaseFast");

    if let Some(target) = zig_target {
        cmd.arg(format!("-Dtarget={target}"));
    }

    cmd.arg("--prefix")
        .arg(prefix)
        .output()
        .expect("failed to invoke zig")
}

fn zig_target_from_rust_target() -> Option<String> {
    let rust_target = std::env::var("TARGET").ok()?;
    let mut parts = rust_target.split('-');
    let arch = parts.next()?;

    if rust_target.ends_with("-linux-ohos") {
        return Some(format!("{arch}-linux-ohos"));
    }

    None
}

fn print_build_output(output: &std::process::Output) {
    if !output.stdout.is_empty() {
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

fn maybe_patch_ziglyph_build_for_zig_016(stderr: &str) -> bool {
    if !stderr.contains("no field named 'root_source_file' in struct 'Build.ExecutableOptions'") {
        return false;
    }

    let path = extract_ziglyph_build_path(stderr).expect("expected ziglyph build.zig path in zig error output");
    if !path.exists() {
        return false;
    }

    std::fs::write(&path, ziglyph_build_script_for_zig_016()).expect("failed to patch ziglyph build.zig");
    true
}

fn extract_ziglyph_build_path(stderr: &str) -> Option<PathBuf> {
    for line in stderr.lines() {
        if !line.contains("ziglyph-") || !line.contains("/build.zig:") {
            continue;
        }
        let idx = line.find(':')?;
        let path = &line[..idx];
        if path.ends_with("/build.zig") {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn ziglyph_build_script_for_zig_016() -> &'static str {
    r#"const std = @import("std");

pub fn build(b: *std.Build) void {
    const optimize = b.standardOptimizeOption(.{});
    const target = b.standardTargetOptions(.{});

    // Export module
    _ = b.addModule("ziglyph", .{ .root_source_file = b.path("src/ziglyph.zig") });

    // Fetch Unicode files step.
    const fetch_exe = b.addExecutable(.{
        .name = "fetch_unicode_files",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/fetch_unicode_files.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const run_fetch_exe = b.addRunArtifact(fetch_exe);
    run_fetch_exe.step.dependOn(&fetch_exe.step);
    if (b.args) |args| run_fetch_exe.addArgs(args);

    const fetch_step = b.step("fetch", "Fetch Unicode files from the Internet.");
    fetch_step.dependOn(&run_fetch_exe.step);

    // Generate Zig files step.
    const gen_exe = b.addExecutable(.{
        .name = "gen_zig_code",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/gen_zig_code.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    gen_exe.step.dependOn(&run_fetch_exe.step);

    const run_gen_exe = b.addRunArtifact(gen_exe);
    run_gen_exe.step.dependOn(&gen_exe.step);
    if (b.args) |args| run_gen_exe.addArgs(args);

    // Fmt step
    const gen_fmt = b.addFmt(.{ .paths = &.{"src"} });
    gen_fmt.step.dependOn(&run_gen_exe.step);

    const gen_step = b.step("gen", "Generate Zig code from Unicode files.");
    gen_step.dependOn(&gen_fmt.step);

    // Main tests
    const main_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/tests.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    // Main tests run step
    const run_main_tests = b.addRunArtifact(main_tests);
    // Main tests top level step
    const test_step = b.step("test", "Run library tests");
    test_step.dependOn(&run_main_tests.step);

    // Internal tests
    const unicode_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/unicode_tests.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    // Main tests run step
    const run_unicode_tests = b.addRunArtifact(unicode_tests);
    // Main tests top level step
    const unicode_test_step = b.step("unicode-test", "Run Unicode tests.");
    unicode_test_step.dependOn(&run_unicode_tests.step);

    // allkeys.txt compression
    const ak_exe = b.addExecutable(.{
        .name = "compress_allkeys",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/akcompress.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    const run_ak_exe = b.addRunArtifact(ak_exe);
    run_ak_exe.step.dependOn(&ak_exe.step);
    if (b.args) |args| run_ak_exe.addArgs(args);

    const ak_step = b.step("akcompress", "Compress tailored allkeys.txt file.");
    ak_step.dependOn(&run_ak_exe.step);
}
"#
}

fn find_zig(workspace_root: &std::path::Path) -> PathBuf {
    if let Some(path) = std::env::var_os("ZIG") {
        return PathBuf::from(path);
    }

    if Command::new("zig").arg("version").output().is_ok() {
        return PathBuf::from("zig");
    }

    workspace_root.join(".context/zig/zig")
}
