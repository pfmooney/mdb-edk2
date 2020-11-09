use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

extern crate goblin;
extern crate memmap;
extern crate pico_args;

fn parse_args() -> Option<(PathBuf, PathBuf)> {
    let mut args = pico_args::Arguments::from_env();

    let obj_path: PathBuf = args.value_from_str("-d").ok()?;
    let dbg_output: PathBuf = args.free_from_str().ok()??;
    Some((dbg_output, obj_path))
}

fn usage() -> ! {
    println!("usage: mdb-tianocore -d <obj path> <debug output file>");
    std::process::exit(0);
}

fn process_file(base: &str, path: &Path, addr: u64) -> Result<()> {
    if !path.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidData, "bad object file"));
    }
    let map = unsafe { memmap::Mmap::map(&File::open(path)?)? };
    let elf = goblin::elf::Elf::parse(&map)
        .or_else(|e| Err(Error::new(ErrorKind::InvalidData, e.to_string())))?;

    let text_shndx = elf
        .section_headers
        .iter()
        .enumerate()
        .find(|(_ndx, hdr)| {
            if let Some(Ok(shdr_name)) = elf.shdr_strtab.get(hdr.sh_name) {
                shdr_name == ".text"
            } else {
                false
            }
        })
        .map(|(ndx, _hdr)| ndx)
        .unwrap_or_else(|| usize::MAX);

    for sym in elf.syms.iter() {
        if sym.is_function() {
            if let Some(Ok(name)) = elf.strtab.get(sym.st_name) {
                println!(
                    "{:x}::nmadd -f -s {:x} \"{}`{}\"",
                    addr + sym.st_value,
                    sym.st_size,
                    base,
                    name
                );
            }
        } else if sym.st_bind() == goblin::elf::sym::STB_GLOBAL
            && sym.st_shndx == text_shndx
        {
            // Functions implemented in assembly may not be properly typed
            if let Some(Ok(name)) = elf.strtab.get(sym.st_name) {
                println!(
                    "{:x}::nmadd -o -s {:x} \"{}`{}\"",
                    addr + sym.st_value,
                    sym.st_size,
                    base,
                    name
                );
            }
        }
    }
    Ok(())
}

fn main() {
    let (dbg, obj_dir) = parse_args().unwrap_or_else(|| usage());

    if !obj_dir.metadata().unwrap_or_else(|_| usage()).is_dir() {
        usage();
    }

    let fp = File::open(dbg).unwrap();
    let bufr = BufReader::new(fp);
    let mut map = BTreeMap::new();

    for line in bufr.lines().map(|l| l.unwrap()) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Follow along as modules are loaded:
        // "Loading <something> at 0x<address> EntryPoint=0x<entry> <file>.efi"
        if let (Some(&"Loading"), Some(&"at"), Some(addr), Some(file)) =
            (fields.get(0), fields.get(2), fields.get(3), fields.get(5))
        {
            if addr.starts_with("0x") && file.ends_with(".efi") {
                if let Ok(addr_parsed) =
                    u64::from_str_radix(addr.trim_start_matches("0x"), 16)
                {
                    map.insert(
                        addr_parsed,
                        file.trim_end_matches(".efi").to_string(),
                    );
                }
            }
            continue;
        }
        // Handle cases where an image load/start fails:
        // "Error: Image at <addr> start failed: ..."
        if line.starts_with("Error: Image at ") && fields.get(3).is_some() {
            if let Ok(addr_parsed) =
                u64::from_str_radix(fields.get(3).unwrap(), 16)
            {
                map.remove(&addr_parsed);
            }
            continue;
        }
    }
    for (addr_offset, file_base) in map.iter() {
        let obj = obj_dir.join(format!("{}.debug", file_base));
        let err = process_file(file_base, &obj, *addr_offset);
    }
}
