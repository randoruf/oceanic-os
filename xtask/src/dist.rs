use std::{env, error::Error, fs, path::Path, process::Command};

use structopt::StructOpt;

use crate::{BOOTFS, H2O_BOOT, H2O_KERNEL, H2O_SYSCALL, H2O_TINIT, OC_LIB};

#[derive(Debug, StructOpt)]
pub enum Type {
    Img,
}

#[derive(Debug, StructOpt)]
pub struct Dist {
    #[structopt(subcommand)]
    ty: Type,
    #[structopt(long = "--release", parse(from_flag))]
    release: bool,
}

impl Dist {
    pub fn build(self) -> Result<(), Box<dyn Error>> {
        let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let src_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(1)
            .unwrap();
        let target_root = env::var("CARGO_TARGET_DIR")
            .unwrap_or_else(|_| src_root.join("target").to_string_lossy().to_string());

        // Generate syscall stubs
        crate::gen::gen_syscall(
            src_root.join(H2O_KERNEL).join("syscall"),
            src_root.join(H2O_KERNEL).join("target/wrapper.rs"),
            src_root.join("h2o/libs/syscall/target/call.rs"),
            src_root.join("h2o/libs/syscall/target/stub.rs"),
        )?;

        // Build h2o_boot
        self.build_impl(
            &cargo,
            "h2o_boot.efi",
            "BootX64.efi",
            src_root.join(H2O_BOOT),
            Path::new(&target_root).join("x86_64-unknown-uefi"),
            &target_root,
        )?;

        // Build the VDSO
        {
            let target_triple = src_root.join(".cargo/x86_64-pc-oceanic.json");
            let cd = src_root.join(H2O_SYSCALL);
            let ldscript = cd.join("syscall.ld");

            fs::copy(cd.join("target/rxx.rs.in"), cd.join("target/rxx.rs"))?;

            let mut cmd = Command::new(&cargo);
            let cmd = cmd.current_dir(&cd).arg("rustc").args([
                "--crate-type=cdylib",
                &format!("--target={}", target_triple.to_string_lossy()),
                "-Zunstable-options",
                "-Zbuild-std=core,compiler_builtins,alloc,panic_abort",
                "-Zbuild-std-features=compiler-builtins-mem",
                "--release", /* VDSO can always be the release version and discard the debug
                              * symbols. */
                "--no-default-features",
                "--features",
                "call",
            ]);
            cmd.args([
                "--",
                &format!("-Clink-arg=-T{}", ldscript.to_string_lossy()),
            ])
            .status()?
            .exit_ok()?;

            // Copy the binary to target.
            let bin_dir = Path::new(&target_root).join("x86_64-pc-oceanic/release");
            fs::copy(
                bin_dir.join("libsv_call.so"),
                src_root.join(H2O_KERNEL).join("target/vdso"),
            )?;

            fs::File::create(cd.join("target/rxx.rs"))?;
        }

        // Build h2o_kernel
        self.build_impl(
            &cargo,
            "h2o",
            "KERNEL",
            src_root.join(H2O_KERNEL),
            Path::new(&target_root).join("x86_64-h2o-kernel"),
            &target_root,
        )?;

        // Build h2o_tinit
        self.build_impl(
            &cargo,
            "tinit",
            "TINIT",
            src_root.join(H2O_TINIT),
            Path::new(&target_root).join("x86_64-h2o-tinit"),
            &target_root,
        )?;

        self.build_lib(&cargo, &src_root, &target_root)?;

        crate::gen::gen_bootfs(Path::new(BOOTFS).join("../BOOT.fs"))?;

        // Generate debug symbols
        println!("Generating debug symbols");
        Command::new("sh")
            .current_dir(src_root)
            .arg("scripts/gendbg.sh")
            .status()?
            .exit_ok()?;

        match &self.ty {
            Type::Img => {
                // Generate img
                println!("Generating a hard disk image file");
                Command::new("sh")
                    .current_dir(src_root)
                    .arg("scripts/genimg.sh")
                    .status()?
                    .exit_ok()?;
            }
        }
        Ok(())
    }

    fn build_lib(
        &self,
        cargo: &str,
        src_root: impl AsRef<Path>,
        target_root: &str,
    ) -> Result<(), Box<dyn Error>> {
        let src_root = src_root.as_ref().join(OC_LIB);
        let bin_dir = Path::new(target_root).join("x86_64-pc-oceanic");
        let dst_root = Path::new(target_root).join("bootfs/lib");

        self.build_impl(
            cargo,
            "libldso.so",
            "ld-oceanic.so",
            src_root.join("libc/ldso"),
            &bin_dir,
            &dst_root,
        )?;

        Ok(())
    }

    fn build_impl(
        &self,
        cargo: &str,
        src_name: &str,
        dst_name: &str,
        src_dir: impl AsRef<Path>,
        bin_dir: impl AsRef<Path>,
        target_dir: impl AsRef<Path>,
    ) -> Result<(), Box<dyn Error>> {
        println!("Building {}", dst_name);

        let mut cmd = Command::new(cargo);
        let cmd = cmd.current_dir(src_dir).arg("build");
        if self.release {
            cmd.arg("--release");
        }
        cmd.status()?.exit_ok()?;
        let bin_dir = if self.release {
            bin_dir.as_ref().join("release")
        } else {
            bin_dir.as_ref().join("debug")
        };
        fs::copy(bin_dir.join(src_name), target_dir.as_ref().join(dst_name))?;
        Ok(())
    }
}
