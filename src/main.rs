//! `aomesh` — bake per-vertex ambient occlusion into a 1D texture.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

use aomesh::{BakeOptions, ao, bake, preview};

/// Bake per-vertex ambient occlusion into 1D texture coordinates.
///
/// Each vertex is tested against the viewpoints on a sphere around the mesh that lie
/// in front of its surface — the 180° span its normal opens into — and the `cos θ`
/// weighted share of those that reach it becomes its texture coordinate, so grazing
/// viewpoints carry no weight and an unobstructed surface is pure white while only real
/// geometry darkens it. A 256 x 1 black-to-white PNG written beside the mesh turns that
/// coordinate into an ordinary diffuse texture any renderer can display.
#[derive(Parser, Debug)]
#[command(name = "aomesh", version, about)]
struct Args {
    /// Wavefront OBJ to bake. Left untouched; outputs are new files.
    input: PathBuf,

    /// Destination OBJ. Defaults to the input's name with an `_ao` suffix. The MTL
    /// and PNG follow the same stem, in the same directory.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Viewpoints on the enclosing sphere. Each vertex is tested against the ones in
    /// front of its normal, about half of these. More resolve finer creases; the cost
    /// is linear.
    #[arg(
        short,
        long,
        default_value_t = NonZeroUsize::new(ao::DEFAULT_SAMPLES).expect("default is non-zero")
    )]
    samples: NonZeroUsize,

    /// `newmtl` name in the generated library.
    #[arg(long, default_value = aomesh::DEFAULT_MATERIAL_NAME)]
    material: String,

    /// Edge length in pixels of each panel of the validation image.
    #[arg(long, default_value_t = preview::DEFAULT_PREVIEW_SIZE)]
    preview_size: usize,

    /// Skip the validation image.
    #[arg(long)]
    no_preview: bool,

    /// Report only what was written, not how the bake measured.
    #[arg(short, long)]
    quiet: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aomesh: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let options = BakeOptions {
        samples: args.samples.get(),
        material_name: args.material.clone(),
    };
    let report = bake(&args.input, args.output.as_deref(), &options)?;

    if !args.quiet {
        println!(
            "{}: {} vertices, {} triangles, {} viewpoints",
            args.input.display(),
            report.vertices,
            report.triangles,
            report.samples,
        );
        println!(
            "visibility  min {:.3}  mean {:.3}  max {:.3}",
            report.visibility.min, report.visibility.mean, report.visibility.max
        );
    }
    println!("wrote {}", report.object.display());
    println!("wrote {}", report.material.display());
    println!("wrote {}", report.texture.display());

    if !args.no_preview {
        let image = preview_path(&report.object);
        preview::render_preview(
            &args.input,
            &report.object,
            &image,
            args.preview_size.max(16),
        )?;
        println!("wrote {}", image.display());
    }

    #[cfg(feature = "count")]
    {
        use aomesh::bvh::counters as c;
        use std::sync::atomic::Ordering::Relaxed;
        let g = |s: &std::sync::atomic::AtomicU64| s.load(Relaxed) as f64;
        let (rays, pops, crossed) = (g(&c::RAYS), g(&c::POPS), g(&c::BOXES_CROSSED));
        let (leaves, tris, skip) = (g(&c::LEAVES), g(&c::TRIS), g(&c::SKIPPED_INCIDENT));
        let (mt, hits) = (g(&c::MT), g(&c::HITS));
        let per = |n: f64| if rays > 0.0 { n / rays } else { 0.0 };
        eprintln!(
            "rays {:.0} | pops/ray {:.2} (crossed {:.2}) | leaves/ray {:.2} | tris/ray {:.2} \
             (incident-skip {:.2}, MT {:.2}) | blocked {:.1}%",
            rays,
            per(pops),
            per(crossed),
            per(leaves),
            per(tris),
            per(skip),
            per(mt),
            if rays > 0.0 { 100.0 * hits / rays } else { 0.0 },
        );
    }

    if !args.quiet {
        println!("done in {:.2}s", started.elapsed().as_secs_f64());
    }
    Ok(())
}

/// The validation image for a baked OBJ: `<stem>_preview.png` beside it.
fn preview_path(object: &Path) -> PathBuf {
    let stem = object.file_stem().unwrap_or_default().to_string_lossy();
    object.with_file_name(format!("{stem}_preview.png"))
}
