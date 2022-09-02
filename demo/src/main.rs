use std::time::Instant;

use clap::Parser;
use env_logger::Env;
use jitfive::{
    backend::llvm::{to_jit_fn, JitContext},
    compiler::Compiler,
    context::Context,
    eval::{Eval, EVAL_ARRAY_SIZE},
    render::render,
};
use log::{error, info, warn};

/// Simple test program
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Render `.dot` files representing compilation
    #[clap(short, long)]
    dot: bool,

    /// Name of a `.png` file to write
    #[clap(short, long)]
    image: Option<String>,

    /// Render using the LLVM JIT-compiled function
    #[clap(short, long, requires = "image")]
    jit: bool,

    /// Use the interpreter
    #[clap(long, requires = "image", conflicts_with = "jit")]
    interpreter: bool,

    /// Render using the `dynasm`-compiled function
    #[clap(short, long, requires = "image",
           conflicts_with_all = &["jit", "interpreter"])]
    asm: bool,

    /// Use per-pixel, brute-force rendering
    #[clap(short, long, requires_all = &["image", "jit"])]
    brute: bool,

    /// Image size
    #[clap(short, long, requires = "image", default_value = "128")]
    size: u32,

    /// Name of the model file to load
    filename: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info"))
        .init();

    let now = Instant::now();
    let args = Args::parse();
    let mut file = std::fs::File::open(args.filename)?;
    let (ctx, root) = Context::from_text(&mut file)?;
    info!("Loaded file in {:?}", now.elapsed());

    let now = Instant::now();
    let compiler = Compiler::new(&ctx, root);
    info!("Built Compiler in {:?}", now.elapsed());

    if args.dot {
        info!("Saving .dot files");
        std::fs::write("stage0.dot", compiler.stage0_dot())?;
        std::fs::write("stage1.dot", compiler.stage1_dot())?;
        std::fs::write("stage2.dot", compiler.stage2_dot())?;
        std::fs::write("stage3.dot", compiler.stage3_dot())?;
        for i in 0..4 {
            info!("Converting stage{}.dot to PDF", i);
            let r = std::process::Command::new("dot")
                .arg("-T")
                .arg("pdf")
                .arg("-o")
                .arg(format!("stage{}.pdf", i))
                .arg(format!("stage{}.dot", i))
                .output();
            match r {
                Ok(v) => {
                    if !v.status.success() {
                        error!("dot exited with error code {:?}", v.status);
                    }
                    let stdout = std::str::from_utf8(&v.stdout).unwrap();
                    let stderr = std::str::from_utf8(&v.stderr).unwrap();
                    if !stdout.is_empty() {
                        info!("`dot` stdout:");
                        for line in stdout.lines() {
                            info!("    {}", line);
                        }
                    }
                    if !stderr.is_empty() {
                        warn!("`dot` stderr:");
                        for line in stderr.lines() {
                            warn!("    {}", line);
                        }
                    }
                }
                Err(e) => {
                    warn!("Could not execute `dot`: {:?}", e);
                    break;
                }
            }
        }
    }

    /*
    if let Some(m) = args.metal {
        std::fs::write(m, prog.to_metal(jitfive::metal::Mode::Interval))?
    }
    */
    if let Some(img) = args.image {
        let mut now = None;
        let buffer: Vec<u8> = if args.jit {
            let jit_ctx = JitContext::new();
            let jit_fn = to_jit_fn(&compiler, &jit_ctx)?;
            now = Some(Instant::now());
            if args.brute {
                let choices = vec![u32::MAX; (compiler.num_choices + 15) / 16];
                let scale = args.size;
                let mut out = Vec::with_capacity((scale * scale) as usize);
                let div = (scale - 1) as f64;
                for i in 0..scale {
                    let y = -(-1.0 + 2.0 * (i as f64) / div);
                    const N: u32 = EVAL_ARRAY_SIZE as u32;
                    for j in 0..((scale + N - 1) / N) {
                        let y_array = [y as f32; N as usize];
                        let mut x_array = [0.0; N as usize];
                        for k in 0..N {
                            let x = j * N + k;
                            if x < scale {
                                x_array[k as usize] =
                                    (-1.0 + 2.0 * (x as f64) / div) as f32;
                            }
                        }
                        let v = jit_fn.array(x_array, y_array, &choices);
                        for k in 0..N {
                            if j * N + k < scale {
                                out.push(if v[k as usize] <= 0.0 {
                                    [u8::MAX; 4]
                                } else {
                                    [0, 0, 0, 255]
                                });
                            }
                        }
                    }
                }
                out.into_iter().flat_map(|i| i.into_iter()).collect()
            } else {
                let image = render(args.size as usize, &jit_fn);
                image
                    .into_iter()
                    .flat_map(|p| p.as_color().into_iter())
                    .collect()
            }
        } else if args.interpreter {
            let scale = args.size;
            let mut out = Vec::with_capacity((scale * scale) as usize);
            let scheduled = jitfive::scheduled::schedule(&ctx, root);
            let tape = jitfive::backend::tape32::Tape::new(&scheduled);
            let mut workspace = tape.workspace();

            now = Some(Instant::now());
            let div = (scale - 1) as f64;
            for i in 0..scale {
                let y = -(-1.0 + 2.0 * (i as f64) / div);
                for j in 0..scale {
                    let x = -1.0 + 2.0 * (j as f64) / div;
                    let v = tape.eval(x as f32, y as f32, &mut workspace);
                    out.push(v <= 0.0);
                }
            }

            // Convert from Vec<bool> to an image
            out.into_iter()
                .map(|b| if b { [u8::MAX; 4] } else { [0, 0, 0, 255] })
                .flat_map(|i| i.into_iter())
                .collect()
        } else if args.asm {
            let scale = args.size;
            let mut out = Vec::with_capacity((scale * scale) as usize);

            now = Some(Instant::now());
            let scheduled = jitfive::scheduled::schedule(&ctx, root);
            let tape = jitfive::backend::tape32::Tape::new_with_reg_limit(
                &scheduled,
                jitfive::backend::dynasm::REGISTER_LIMIT,
            );
            let jit = jitfive::backend::dynasm::tape_to_float(&tape);
            let eval = jit.into_eval();
            info!("Built JIT function in {:?}", now.unwrap().elapsed());

            now = Some(Instant::now());
            let i_jit = jitfive::backend::dynasm::tape_to_interval(&tape);
            let i_eval = i_jit.into_eval();
            info!(
                "Built interval JIT function in {:?}",
                now.unwrap().elapsed()
            );
            now = Some(Instant::now());
            println!("{:?}", i_eval.eval([-0.5, 0.0], [-0.5, 0.0]));
            info!("Calculated in {:?}", now.unwrap().elapsed());

            now = Some(Instant::now());
            let div = (scale - 1) as f64;
            for i in 0..scale {
                let y = -(-1.0 + 2.0 * (i as f64) / div);
                for j in 0..scale {
                    let x = -1.0 + 2.0 * (j as f64) / div;
                    let v = eval.eval(x as f32, y as f32);
                    out.push(v <= 0.0);
                }
            }

            // Convert from Vec<bool> to an image
            out.into_iter()
                .map(|b| if b { [u8::MAX; 4] } else { [0, 0, 0, 255] })
                .flat_map(|i| i.into_iter())
                .collect()
        } else {
            now = Some(Instant::now());
            let scale = args.size;
            let mut out = Vec::with_capacity((scale * scale) as usize);
            let div = (scale - 1) as f64;
            for i in 0..scale {
                let y = -(-1.0 + 2.0 * (i as f64) / div);
                for j in 0..scale {
                    let x = -1.0 + 2.0 * (j as f64) / div;
                    let v = ctx.eval_xyz(root, x, y, 0.0)? as f32;
                    out.push(v <= 0.0);
                }
            }

            // Convert from Vec<bool> to an image
            out.into_iter()
                .map(|b| if b { [u8::MAX; 4] } else { [0, 0, 0, 255] })
                .flat_map(|i| i.into_iter())
                .collect()
        };
        info!("Finished rendering in {:?}", now.unwrap().elapsed());

        image::save_buffer(
            img,
            &buffer,
            args.size as u32,
            args.size as u32,
            image::ColorType::Rgba8,
        )?;
    }
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////

/*
mod gpu {
    use super::*;
    use piet_gpu_hal::{Instance, InstanceFlags, Session};

    pub fn render(prog: &Program, size: u32) -> Vec<[u8; 4]> {
        let (instance, _) =
            Instance::new(None, InstanceFlags::empty()).unwrap();

        unsafe {
            let device = instance.device(None).unwrap();
            let session = Session::new(device);
            let mut metal = Render::new(prog, &session);
            for _i in 0..20 {
                metal.do_render(size, &session);
            }
            metal.load_image()
        }
    }
}
*/
