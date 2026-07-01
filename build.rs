use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use clap_mangen::Man;
use std::env;
use std::fs;
use std::io::Result;
use std::path::PathBuf;

#[path = "src/cli.rs"]
mod cli;

use cli::Cli;

fn generate_completions(outdir: &std::ffi::OsString) -> Result<()> {
    let mut cmd = Cli::command();

    // Generate shell completions to OUT_DIR (for cargo build)
    for shell in [
        Shell::Bash,
        Shell::Fish,
        Shell::Zsh,
        Shell::PowerShell,
        Shell::Elvish,
    ] {
        generate_to(shell, &mut cmd, "fatou", outdir)?;
    }

    // Also copy completions to target/completions for packaging
    let completions_dir = PathBuf::from("target/completions");
    fs::create_dir_all(&completions_dir)?;

    let outdir_path = PathBuf::from(outdir);

    // Copy bash, fish, and zsh completions for packaging
    let bash_src = outdir_path.join("fatou.bash");
    let fish_src = outdir_path.join("fatou.fish");
    let zsh_src = outdir_path.join("_fatou");

    if bash_src.exists() {
        fs::copy(&bash_src, completions_dir.join("fatou.bash"))?;
    }
    if fish_src.exists() {
        fs::copy(&fish_src, completions_dir.join("fatou.fish"))?;
    }
    if zsh_src.exists() {
        fs::copy(&zsh_src, completions_dir.join("_fatou"))?;
    }

    Ok(())
}

fn format_see_also(refs: &[String]) -> String {
    let formatted: Vec<String> = refs.iter().map(|r| format!("\\fB{}\\fR(1)", r)).collect();
    format!(".SH \"SEE ALSO\"\n{}\n", formatted.join(", "))
}

fn generate_man_pages() -> Result<()> {
    // Create man directory if it doesn't exist
    let out_dir = PathBuf::from("target/man");
    fs::create_dir_all(&out_dir)?;

    // Generate main man page and all subcommand pages (like git/cargo do)
    let cmd = Cli::command();

    // Collect top-level subcommand names (skip "help") for SEE ALSO sections
    let subcommand_names: Vec<String> = cmd
        .get_subcommands()
        .filter(|s| s.get_name() != "help")
        .map(|s| format!("fatou-{}", s.get_name()))
        .collect();

    // Generate main page
    let man = Man::new(cmd.clone());
    let mut buffer = Vec::new();
    man.render(&mut buffer)?;
    let main_content =
        String::from_utf8_lossy(&buffer).into_owned() + &format_see_also(&subcommand_names);
    fs::write(out_dir.join("fatou.1"), main_content.as_bytes())?;

    // Generate pages for each top-level subcommand
    for subcommand in cmd.get_subcommands() {
        let subcommand_name = subcommand.get_name();
        if subcommand_name == "help" {
            continue; // Skip help command
        }

        let name = format!("fatou-{}", subcommand_name);
        let man = Man::new(subcommand.clone().version(env!("CARGO_PKG_VERSION"))).title(&name);
        let mut buffer = Vec::new();
        man.render(&mut buffer)?;

        // Post-process: fix NAME and SYNOPSIS subcommand references
        let content = String::from_utf8_lossy(&buffer);
        let fixed_content = content
            .replace(
                &format!("{} \\-", subcommand_name),
                &format!("{} \\-", name),
            )
            .replace(
                &format!("\\fB{}\\fR", subcommand_name),
                &format!("\\fBfatou {}\\fR", subcommand_name),
            )
            .replace(
                &format!("{}\\-", subcommand_name),
                &format!("fatou\\-{}\\-", subcommand_name),
            );

        // SEE ALSO: fatou(1) plus sibling subcommand pages
        let mut see_also_refs: Vec<String> = vec!["fatou".to_string()];
        see_also_refs.extend(subcommand_names.iter().filter(|n| *n != &name).cloned());
        let with_see_also = fixed_content + &format_see_also(&see_also_refs);

        fs::write(
            out_dir.join(format!("{}.1", name)),
            with_see_also.as_bytes(),
        )?;
    }

    Ok(())
}

fn main() -> Result<()> {
    // Generate shell completions
    if let Some(outdir) = env::var_os("OUT_DIR") {
        generate_completions(&outdir)?;
    }

    // Generate man pages
    generate_man_pages()?;

    println!("cargo:rerun-if-changed=src/cli.rs");
    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
