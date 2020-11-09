use std::collections::BTreeMap;
use std::fs::File;
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

struct SymRes<'a> {
    name: &'a str,
    size: u64,
    is_func: bool,
}

fn post_process(results: &BTreeMap<u64, SymRes>, base: &str, addr_end: u64) {
    let mut iter = results.iter().peekable();
    while let Some((addr, res)) = iter.next() {
        let size = match res.size {
            0 => {
                // For any entries which lack a proper size, stretch it out
                // until it hits the next entry (or the end of the section).
                if let Some((naddr, _)) = iter.peek() {
                    *naddr - addr
                } else {
                    addr_end - addr
                }
            }
            sz => sz,
        };
        // While '`' would be the expected delimiter between object and function
        // name, it (currently) confuses name resolution in mdb-bhyve since
        // there are effectively no objects.  Use '.' instead, so the private
        // symbols can be referred to directly.
        println!(
            "{:x}::nmadd -{} -s {:x} \"{}.{}\"",
            addr,
            if res.is_func { "f" } else { "o" },
            size,
            base,
            res.name
        );
    }
}

fn process_file(base: &str, path: &Path, addr_start: u64) -> Result<()> {
    if !path.metadata()?.is_file() {
        return Err(Error::new(ErrorKind::InvalidData, "bad object file"));
    }
    let map = unsafe { memmap::Mmap::map(&File::open(path)?)? };
    let elf = goblin::elf::Elf::parse(&map)
        .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;

    let (text_shndx, addr_end) = if let Some((ndx, hdr)) =
        elf.section_headers.iter().enumerate().find(|(_ndx, hdr)| {
            if let Some(Ok(shdr_name)) = elf.shdr_strtab.get(hdr.sh_name) {
                shdr_name == ".text"
            } else {
                false
            }
        }) {
        (ndx, addr_start + hdr.sh_size)
    } else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "No .text section found",
        ));
    };

    let mut results = BTreeMap::new();

    for sym in elf.syms.iter() {
        if sym.st_shndx != text_shndx {
            continue;
        }

        if sym.is_function() {
            if let Some(Ok(name)) = elf.strtab.get(sym.st_name) {
                results.insert(
                    addr_start + sym.st_value,
                    SymRes { name, size: sym.st_size, is_func: true },
                );
            }
        } else if sym.st_bind() == goblin::elf::sym::STB_GLOBAL {
            // Functions implemented in assembly may not be properly typed
            if let Some(Ok(name)) = elf.strtab.get(sym.st_name) {
                results.insert(
                    addr_start + sym.st_value,
                    SymRes { name, size: sym.st_size, is_func: false },
                );
            }
        }
    }
    post_process(&results, base, addr_end);
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
        if let Err(e) = process_file(file_base, &obj, *addr_offset) {
            eprintln!("Error processing {}: {:?}", file_base, e);
        }
    }
}
