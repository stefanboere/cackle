//! This module builds a graph of relationships between symbols and linker sections. Provided code
//! was compiled with one symbol per section, which it should have been, there should be a 1:1
//! relationship between symbols and sections.
//!
//! We also parse the Dwarf debug information to determine what source file each linker section came
//! from.

use crate::checker::Checker;
use crate::checker::SourceLocation;
use crate::checker::Usage;
use crate::problem::ApiUsage;
use crate::problem::ProblemList;
use crate::symbol::Symbol;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use ar::Archive;
use gimli::Dwarf;
use gimli::EndianSlice;
use gimli::LittleEndian;
use object::Object;
use object::ObjectSection;
use object::ObjectSymbol;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filetype {
    Archive,
    Other,
}

#[derive(Default)]
struct ApiUsageCollector {
    outputs: GraphOutputs,

    exe: ExeInfo,
}

/// Information derived from a linked binary. Generally an executable, but could also be shared
/// object (so).
#[derive(Default)]
struct ExeInfo {
    symbol_addresses: HashMap<Symbol, u64>,
}

#[derive(Default)]
pub(crate) struct GraphOutputs {
    api_usages: Vec<ApiUsage>,

    /// Problems not related to api_usage. These can't be fixed by config changes via the UI, since
    /// once computed, they won't be recomputed.
    base_problems: ProblemList,
}

struct ObjectIndex<'obj, 'data> {
    obj: &'obj object::File<'data>,

    /// For each section, stores a symbol defined at the start of that section, if any.
    section_index_to_symbol: Vec<Option<Symbol>>,
}

pub(crate) fn scan_objects(
    paths: &[PathBuf],
    exe_path: &Path,
    checker: &Checker,
) -> Result<GraphOutputs> {
    let file_bytes = std::fs::read(exe_path)
        .with_context(|| format!("Failed to read `{}`", exe_path.display()))?;
    let obj = object::File::parse(file_bytes.as_slice())
        .with_context(|| format!("Failed to parse {}", exe_path.display()))?;
    let owned_dwarf = Dwarf::load(|id| load_section(&obj, id))?;
    let dwarf = owned_dwarf.borrow(|section| gimli::EndianSlice::new(section, gimli::LittleEndian));
    let ctx = addr2line::Context::from_dwarf(dwarf)
        .with_context(|| format!("Failed to process {}", exe_path.display()))?;

    let mut graph = ApiUsageCollector::default();
    graph.exe.load_symbols(&obj)?;
    for path in paths {
        graph
            .process_file(path, &ctx, checker)
            .with_context(|| format!("Failed to process `{}`", path.display()))?;
    }

    Ok(graph.outputs)
}

impl GraphOutputs {
    pub(crate) fn problems(&self, checker: &mut Checker) -> Result<ProblemList> {
        let mut problems = self.base_problems.clone();
        for api_usage in &self.api_usages {
            checker.permission_used(api_usage, &mut problems);
        }

        Ok(problems)
    }
}

impl ApiUsageCollector {
    fn process_file(
        &mut self,
        filename: &Path,
        ctx: &addr2line::Context<EndianSlice<LittleEndian>>,
        checker: &Checker,
    ) -> Result<()> {
        let mut buffer = Vec::new();
        match Filetype::from_filename(filename) {
            Filetype::Archive => {
                let mut archive = Archive::new(File::open(filename)?);
                while let Some(entry_result) = archive.next_entry() {
                    let Ok(mut entry) = entry_result else { continue; };
                    buffer.clear();
                    entry.read_to_end(&mut buffer)?;
                    self.process_object_file_bytes(filename, &buffer, ctx, checker)?;
                }
            }
            Filetype::Other => {
                let file_bytes = std::fs::read(filename)
                    .with_context(|| format!("Failed to read `{}`", filename.display()))?;
                self.process_object_file_bytes(filename, &file_bytes, ctx, checker)?;
            }
        }
        Ok(())
    }

    /// Processes an unlinked object file - as opposed to an executable or a shared object, which
    /// has been linked.
    fn process_object_file_bytes(
        &mut self,
        filename: &Path,
        file_bytes: &[u8],
        ctx: &addr2line::Context<EndianSlice<LittleEndian>>,
        checker: &Checker,
    ) -> Result<()> {
        let obj = object::File::parse(file_bytes)
            .with_context(|| format!("Failed to parse {}", filename.display()))?;
        let object_index = ObjectIndex::new(&obj);
        for section in obj.sections() {
            let Some(section_start_symbol) = object_index
                .section_index_to_symbol
                .get(section.index().0)
                .and_then(Option::as_ref) else {
                    continue;
                };
            let Some(section_start_in_exe) = self.exe.symbol_addresses.get(section_start_symbol) else {
                continue;
            };
            for (offset, rel) in section.relocations() {
                let location = ctx
                    .find_location(section_start_in_exe + offset)
                    .context("find_location failed")?;
                let Some(target_symbol) = object_index.target_symbol(&rel)? else {
                    continue;
                };
                let source_filename = location.and_then(|l| l.file);
                let Some(source_filename) = source_filename else {
                    continue;
                };

                // Ignore sources from the rust standard library and precompiled crates that are bundled
                // with the standard library (e.g. hashbrown).
                let source_filename = Path::new(&source_filename);
                if source_filename.starts_with("/rustc/")
                    || source_filename.starts_with("/cargo/registry")
                {
                    continue;
                }
                let crate_names =
                    checker.crate_names_from_source_path(source_filename, filename)?;
                let mut api_usages = Vec::new();
                for crate_name in crate_names {
                    for name_parts in target_symbol.parts()? {
                        // If a package references another symbol within the same package, ignore
                        // it.
                        if name_parts
                            .first()
                            .map(|name_start| crate_name.as_ref() == name_start)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        let location = SourceLocation {
                            filename: source_filename.to_owned(),
                        };
                        for permission in checker.apis_for_path(&name_parts) {
                            let mut usages = BTreeMap::new();
                            usages.insert(
                                permission.clone(),
                                vec![Usage {
                                    location: location.clone(),
                                    from: section_start_symbol.clone(),
                                    to: target_symbol.clone(),
                                }],
                            );
                            api_usages.push(ApiUsage {
                                crate_name: crate_name.clone(),
                                usages,
                            });
                        }
                    }
                }
                self.outputs.api_usages.append(&mut api_usages);
            }
        }
        Ok(())
    }
}

impl<'obj, 'data> ObjectIndex<'obj, 'data> {
    fn new(obj: &'obj object::File<'data>) -> Self {
        let max_section_index = obj.sections().map(|s| s.index().0).max().unwrap_or(0);
        let mut first_symbol_by_section = vec![None; max_section_index + 1];
        for symbol in obj.symbols() {
            let name = symbol.name_bytes().unwrap_or_default();
            if symbol.address() != 0 || name.is_empty() {
                continue;
            }
            let Some(section_index) = symbol.section_index() else {
                continue;
            };
            first_symbol_by_section[section_index.0] = Some(Symbol::new(name));
        }
        Self {
            obj,
            section_index_to_symbol: first_symbol_by_section,
        }
    }

    fn target_symbol(&self, rel: &object::Relocation) -> Result<Option<Symbol>> {
        let object::RelocationTarget::Symbol(symbol_index) = rel.target() else { bail!("Unsupported relocation kind"); };
        let Ok(symbol) = self.obj.symbol_by_index(symbol_index) else { bail!("Invalid symbol index in object file"); };
        let name = symbol.name_bytes().unwrap_or_default();
        if !name.is_empty() {
            return Ok(Some(Symbol::new(name)));
        }
        let Some(section_index) = symbol.section_index() else {
            bail!("Relocation target has empty name and no section index");
        };
        Ok(self
            .section_index_to_symbol
            .get(section_index.0)
            .ok_or_else(|| anyhow!("Unnamed symbol has invalid section index"))?
            .clone())
    }
}

impl ExeInfo {
    fn load_symbols(&mut self, obj: &object::File) -> Result<()> {
        for symbol in obj.symbols() {
            self.symbol_addresses
                .insert(Symbol::new(symbol.name_bytes()?), symbol.address());
        }
        Ok(())
    }
}

/// Loads section `id` from `obj`.
fn load_section(
    obj: &object::File,
    id: gimli::SectionId,
) -> Result<Cow<'static, [u8]>, gimli::Error> {
    let Some(section) = obj.section_by_name(id.name()) else {
        return Ok(Cow::Borrowed([].as_slice()));
    };
    let Ok(data) = section.uncompressed_data() else {
        return Ok(Cow::Borrowed([].as_slice()));
    };
    // TODO: Now that we're loading binaries rather than object files, we don't apply relocations.
    // We might not need owned data here.
    Ok(Cow::Owned(data.into_owned()))
}

impl Filetype {
    fn from_filename(filename: &Path) -> Self {
        let Some(extension) = filename
        .extension() else {
            return Filetype::Other;
        };
        if extension == "rlib" || extension == ".a" {
            Filetype::Archive
        } else {
            Filetype::Other
        }
    }
}
