use std::io::Write;
use std::{env, fs, path::PathBuf};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let docs_dir = manifest_dir.join("../../docs/src");

    println!("cargo::rerun-if-changed={}", docs_dir.display());

    let mut generated = fs::File::create(out_dir.join("book_doctests.rs")).unwrap();

    discover_md_files(&docs_dir, &docs_dir, &mut generated);
}

fn discover_md_files(base: &PathBuf, dir: &PathBuf, out: &mut fs::File) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            // Skip generated/copied directories that aren't book source
            let name = entry.file_name();
            if name == "rustdoc" {
                continue;
            }
            discover_md_files(base, &path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            let rel = path.strip_prefix(base).unwrap();
            // Create a valid Rust module name from the path
            let mod_name: String = rel
                .to_string_lossy()
                .replace('/', "__")
                .replace(".md", "")
                .replace('-', "_");

            // Use the canonicalized absolute path so include_str! works
            // regardless of where OUT_DIR is located.
            let abs_path = path.canonicalize().unwrap();

            writeln!(out, "#[doc = include_str!(\"{}\")]", abs_path.display(),).unwrap();
            writeln!(out, "mod {mod_name} {{}}\n").unwrap();
        }
    }
}
