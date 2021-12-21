mod error;

pub use tao_syntax::SrcId;

use tao_syntax::{parse_module, ast, SrcNode, Error as SyntaxError};
use tao_analysis::Context as HirContext;
use tao_middle::Context;
use tao_vm::{Program, exec};
use ariadne::sources;
use structopt::StructOpt;
use internment::Intern;
use std::{
    str::FromStr,
    io::Write,
    collections::HashMap,
    fmt,
};
use error::Error;

#[derive(Copy, Clone, Debug)]
pub enum Opt {
    None,
    Fast,
    Size,
}

impl FromStr for Opt {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, &'static str> {
        match s {
            "none" => Ok(Opt::None),
            "fast" => Ok(Opt::Fast),
            "size" => Ok(Opt::Size),
            _ => Err("Optimisation mode does not exist"),
        }
    }
}

impl fmt::Display for Opt {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Opt::None => write!(f, "none"),
            Opt::Fast => write!(f, "fast"),
            Opt::Size => write!(f, "size"),
        }
    }
}

#[derive(Clone, Debug, StructOpt)]
pub struct Options {
    /// Add a debugging layer to stdout (tokens, ast, hir, mir, bytecode)
    #[structopt(long)]
    pub debug: Vec<String>,
    /// Specify an optimisation mode (none, fast, size)
    #[structopt(short, long, default_value = "none")]
    pub opt: Opt,
}

pub fn run<F: FnMut(SrcId) -> Option<String>>(src: String, src_id: SrcId, options: Options, mut writer: impl Write, mut get_file: F) {
    let (mut ast, mut syntax_errors) = parse_module(&src, src_id);

    fn resolve_imports<F: FnMut(SrcId) -> Option<String>>(
        module: Option<&mut ast::Module>,
        imported: &mut HashMap<SrcId, String>,
        import_errors: &mut Vec<Error>,
        syntax_errors: &mut Vec<SyntaxError>,
        get_file: &mut F,
    ) {
        if let Some(module) = module {
            let imports = std::mem::take(&mut module.imports);

            for import in imports {
                let src_id = SrcId::from_path(import.as_str());
                match get_file(src_id) {
                    Some(src) => {
                        // Check for cycles
                        if imported.insert(src_id, src.clone()).is_none() {
                            let (mut ast, mut new_syntax_errors) = parse_module(&src, src_id);
                            syntax_errors.append(&mut new_syntax_errors);

                            resolve_imports(ast.as_deref_mut(), imported, import_errors, syntax_errors, get_file);

                            if let Some(mut ast) = ast {
                                let mut old_items = std::mem::take(&mut module.items);
                                module.items.append(&mut ast.items);
                                module.items.append(&mut old_items);
                            }
                        }
                    },
                    None => import_errors.push(Error::CannotImport(import.clone())),
                }
            }
        }
    }

    // Resolve imports
    let mut imported = HashMap::new();
    let mut import_errors = Vec::new();
    resolve_imports(ast.as_deref_mut(), &mut imported, &mut import_errors, &mut syntax_errors, &mut get_file);
    let mut srcs = sources(imported.into_iter().chain(std::iter::once((src_id, src))));
    if !import_errors.is_empty() {
        for e in import_errors {
            e.write(&mut srcs, &mut writer);
        }
        return;
    }

    let mut syntax_error = false;
    for e in syntax_errors {
        syntax_error = true;
        e.write(&mut srcs, &mut writer);
    }

    // println!("Items = {}", ast.as_ref().unwrap().items.len());
    if options.debug.contains(&"ast".to_string()) {
        writeln!(writer, "{:?}", ast).unwrap();
    }

    if let Some(ast) = ast {
        let (ctx, mut analysis_errors) = HirContext::from_module(&ast);

        if options.debug.contains(&"hir".to_string()) {
            for (_, def) in ctx.defs.iter() {
                writeln!(writer, "{} = {:?}", *def.name, def.body).unwrap();
            }
        }

        if !analysis_errors.is_empty() || syntax_error {
            for e in analysis_errors {
                e.write(&ctx, &mut srcs, &mut writer);
            }
        } else {
            let (concrete, mut con_errors) = ctx.concretize();

            if !con_errors.is_empty() {
                for e in con_errors {
                    e.write(&ctx, &mut srcs, &mut writer);
                }
            } else {
                // let (mut ctx, errors) = Context::from_hir(&ctx);
                let mut ctx = Context::from_concrete(&ctx, &concrete);

                // for err in errors {
                //     err.write(&ctx, srcs, &mut writer);
                // }

                match options.opt {
                    Opt::None => {},
                    Opt::Fast => ctx.optimize(),
                    Opt::Size => todo!("Implement size optimization"),
                }

                if options.debug.contains(&"mir".to_string()) {
                    for (id, proc) in ctx.procs.iter() {
                        writeln!(writer, "PROCEDURE {:?}\n\n{}\n", id, proc.body.print()).unwrap();
                    }
                }

                let prog = Program::from_mir(&ctx);

                if options.debug.contains(&"bytecode".to_string()) {
                    prog.write(&mut writer);
                }
                // prog.write(std::io::stdout());

                if let Some(result) = exec(&prog) {
                    writeln!(writer, "{}", result).unwrap();
                }
            }
        }
    }
}
