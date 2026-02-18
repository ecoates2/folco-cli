use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use folco_core::{
    color::FolderColor,
    progress::{progress_channel, Progress},
    CustomizationContextBuilder, CustomizationProfile, DecalSettings,
    OverlaySettings, SerializablePosition, SerializableSvgSource,
};

#[derive(Parser)]
#[command(name = "folco")]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Show full error chains instead of just the root cause
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Customize folder icons with a profile
    #[command(group(
        clap::ArgGroup::new("customization")
            .required(true)
            .args(["profile", "color", "decal", "overlay"])
            .multiple(true)
    ))]
    Customize {
        /// Directories to customize
        #[arg(required = true)]
        directories: Vec<PathBuf>,

        /// JSON-serialized CustomizationProfile (alternative to individual options)
        #[arg(long, value_name = "JSON")]
        profile: Option<String>,

        // === HSL Mutation Options ===
        /// Folder color
        #[arg(long, value_enum, value_name = "COLOR")]
        color: Option<FolderColor>,

        // === Decal Options ===
        /// Decal source: an SVG file path or raw SVG markup. This gets centered on the folder and tinted to a slightly darker color.
        #[arg(long, value_name = "SOURCE")]
        decal: Option<String>,

        /// Decal scale factor (0.0-1.0)
        #[arg(long, value_name = "SCALE", default_value = "0.70")]
        decal_scale: f32,

        // === Overlay Options ===
        /// Overlay source: an SVG file path, raw SVG markup, emoji character, or
        /// emoji name (e.g. "duck").
        /// See https://emojibase.dev/emojis for accepted emoji names
        #[arg(long, value_name = "SOURCE")]
        overlay: Option<String>,

        /// Overlay position
        #[arg(long, value_name = "POSITION", default_value = "center")]
        overlay_position: PositionArg,

        /// Overlay scale factor (0.0-1.0)
        #[arg(long, value_name = "SCALE", default_value = "0.70")]
        overlay_scale: f32,
    },

    /// Reset folder icons to system default
    Reset {
        /// Directories to reset
        #[arg(required = true)]
        directories: Vec<PathBuf>,
    },

    /// Print the JSON Schema for CustomizationProfile
    Schema,
}

/// Resolve an SVG source string (for decals — only SVG file paths and raw markup).
fn resolve_svg_source(input: &str) -> Result<String> {
    let trimmed = input.trim();

    // Raw SVG markup
    if trimmed.starts_with('<') {
        return Ok(trimmed.to_string());
    }

    // File path
    let path = Path::new(trimmed);
    if path.exists() {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read SVG file: {}", path.display()))?;
        return Ok(contents);
    }

    bail!(
        "Could not resolve decal source {:?}: not a file path or SVG markup. \
         Raw SVG should start with '<'.",
        input
    )
}

/// Returns true if the string contains at least one emoji character.
fn looks_like_emoji(s: &str) -> bool {
    s.chars().any(|c| {
        // Common emoji ranges (supplementary symbols, emoticons, dingbats, etc.)
        matches!(c,
            '\u{200D}'              // ZWJ
            | '\u{FE0F}'           // variation selector
            | '\u{20E3}'           // combining enclosing keycap
            | '\u{2600}'..='\u{27BF}'   // misc symbols & dingbats
            | '\u{2B50}'..='\u{2B55}'   // stars, circles
            | '\u{1F000}'..='\u{1FAFF}' // all major emoji blocks
        )
    })
}

/// Resolve an overlay source string (SVG, emoji character, emoji name, or file path).
fn resolve_overlay_source(input: &str) -> Result<SerializableSvgSource> {
    let trimmed = input.trim();

    // Raw SVG markup
    if trimmed.starts_with('<') {
        return Ok(SerializableSvgSource::from_svg(trimmed));
    }

    // File path (must exist on disk and have an SVG-like extension)
    let path = Path::new(trimmed);
    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) && path.exists() {
        let svg = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read overlay SVG file: {}", path.display()))?;
        return Ok(SerializableSvgSource::from_svg(svg));
    }

    // Emoji character (contains actual emoji codepoints)
    if looks_like_emoji(trimmed) {
        return Ok(SerializableSvgSource::from_emoji(trimmed));
    }

    // Fallback: treat as an emoji name (e.g. "duck", "star", "heart")
    Ok(SerializableSvgSource::from_emoji_name(trimmed))
}

#[derive(Clone, ValueEnum, Default)]
enum PositionArg {
    BottomLeft,
    #[default]
    BottomRight,
    TopLeft,
    TopRight,
    Center,
}

impl From<PositionArg> for SerializablePosition {
    fn from(pos: PositionArg) -> Self {
        match pos {
            PositionArg::BottomLeft => SerializablePosition::BottomLeft,
            PositionArg::BottomRight => SerializablePosition::BottomRight,
            PositionArg::TopLeft => SerializablePosition::TopLeft,
            PositionArg::TopRight => SerializablePosition::TopRight,
            PositionArg::Center => SerializablePosition::Center,
        }
    }
}

fn create_progress_bar(total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{wide_bar} {pos}/{len} {msg}")
            .expect("invalid progress bar template"),
    );
    pb
}



#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Customize {
            directories,
            profile,
            color,
            decal,
            decal_scale,
            overlay,
            overlay_position,
            overlay_scale,
        } => {
            let profile = if let Some(json) = profile {
                // Parse JSON profile
                CustomizationProfile::from_json(&json)
                    .context("Failed to parse CustomizationProfile JSON")?
            } else {
                // Build profile from individual options
                let mut p = CustomizationProfile::new();

                // HSL mutation from --color preset
                if let Some(color) = color {
                    p = p.with_hsl_mutation(color.to_hsl_mutation_settings());
                }

                // Decal
                if let Some(ref source) = decal {
                    let svg = resolve_svg_source(source)?;
                    p = p.with_decal(DecalSettings {
                        source: SerializableSvgSource::from_svg(svg),
                        scale: decal_scale,
                        enabled: true,
                    });
                }

                // Overlay
                if let Some(ref source) = overlay {
                    let source = resolve_overlay_source(source)?;
                    p = p.with_overlay(OverlaySettings {
                        source,
                        position: overlay_position.into(),
                        scale: overlay_scale,
                        enabled: true,
                    });
                }

                p
            };

            customize_folders(directories, profile, cli.verbose).await?;
        }

        Commands::Reset { directories } => {
            reset_folders(directories, cli.verbose).await?;
        }

        Commands::Schema => {
            let schema = CustomizationProfile::json_schema_string()
                .context("Failed to generate JSON schema")?;
            println!("{schema}");
        }
    }

    Ok(())
}

async fn customize_folders(directories: Vec<PathBuf>, profile: CustomizationProfile, verbose: bool) -> Result<()> {
    println!("Initializing...");

    let mut ctx = CustomizationContextBuilder::new()
        .build()
        .context("Failed to initialize customization context")?;

    let (tx, mut rx) = progress_channel(32);

    let total = directories.len() as u64;
    let pb = create_progress_bar(total);

    // Spawn progress handler
    let progress_handle = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            match progress {
                Progress::Started { total } => {
                    pb.set_length(total as u64);
                    pb.set_message("Starting...");
                }
                Progress::Rendering => {
                    pb.set_message("Rendering icons...");
                }
                Progress::RenderFailed { error } => {
                    pb.suspend(|| {
                        if verbose {
                            eprintln!("Render failed: {}", error);
                        } else {
                            eprintln!("Render failed");
                        }
                    });
                }
                Progress::Processing { path, .. } => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    pb.set_message(format!("Processing: {}", name));
                }
                Progress::FolderComplete { .. } => {
                    pb.inc(1);
                }
                Progress::FolderFailed { path, error, .. } => {
                    pb.inc(1);
                    pb.suspend(|| {
                        if verbose {
                            eprintln!("Failed {}: {}", path.display(), error);
                        } else {
                            eprintln!("Failed {}", path.display());
                        }
                    });
                }
                Progress::Completed { succeeded, failed } => {
                    pb.finish_with_message(format!(
                        "Completed: {} succeeded, {} failed",
                        succeeded, failed
                    ));
                }
            }
        }
    });

    // Run customization
    ctx.customize_folders_async(directories, &profile, tx).await;

    // Wait for progress handler to finish
    progress_handle.await?;

    Ok(())
}

async fn reset_folders(directories: Vec<PathBuf>, verbose: bool) -> Result<()> {
    println!("Initializing...");

    let ctx = CustomizationContextBuilder::new()
        .build()
        .context("Failed to initialize customization context")?;

    let (tx, mut rx) = progress_channel(32);

    let total = directories.len() as u64;
    let pb = create_progress_bar(total);

    // Spawn progress handler
    let progress_handle = tokio::spawn(async move {
        while let Some(progress) = rx.recv().await {
            match progress {
                Progress::Started { total } => {
                    pb.set_length(total as u64);
                    pb.set_message("Starting...");
                }
                Progress::Processing { path, .. } => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    pb.set_message(format!("Resetting: {}", name));
                }
                Progress::FolderComplete { .. } => {
                    pb.inc(1);
                }
                Progress::FolderFailed { path, error, .. } => {
                    pb.inc(1);
                    pb.suspend(|| {
                        if verbose {
                            eprintln!("Failed {}: {}", path.display(), error);
                        } else {
                            eprintln!("Failed {}", path.display());
                        }
                    });
                }
                Progress::Completed { succeeded, failed } => {
                    pb.finish_with_message(format!(
                        "Completed: {} succeeded, {} failed",
                        succeeded, failed
                    ));
                }
                _ => {}
            }
        }
    });

    // Run reset
    ctx.reset_folders_async(directories, tx).await;

    // Wait for progress handler to finish
    progress_handle.await?;

    Ok(())
}
