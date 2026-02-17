use std::path::PathBuf;

use anyhow::{Context, Result};
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
    Customize {
        /// Directories to customize
        #[arg(required = true)]
        directories: Vec<PathBuf>,

        /// JSON-serialized CustomizationProfile (alternative to individual options)
        #[arg(long, value_name = "JSON")]
        profile: Option<String>,

        // === HSL Mutation Options ===
        /// Folder color preset
        #[arg(long, value_name = "COLOR")]
        color: Option<FolderColor>,

        // === Decal Options ===
        /// SVG data for decal
        #[arg(long, value_name = "SVG", conflicts_with = "decal_svg_file")]
        decal_svg: Option<String>,

        /// Path to SVG file for decal
        #[arg(long, value_name = "FILE", conflicts_with = "decal_svg")]
        decal_svg_file: Option<PathBuf>,

        /// Decal scale factor (0.0-1.0)
        #[arg(long, value_name = "SCALE", default_value = "0.70")]
        decal_scale: f32,

        // === Overlay Options ===
        /// SVG data for overlay
        #[arg(long, value_name = "SVG", conflicts_with_all = ["overlay_emoji", "overlay_emoji_name", "overlay_svg_file"])]
        overlay_svg: Option<String>,

        /// Path to SVG file for overlay
        #[arg(long, value_name = "FILE", conflicts_with_all = ["overlay_svg", "overlay_emoji", "overlay_emoji_name"])]
        overlay_svg_file: Option<PathBuf>,

        /// Emoji character for overlay (rendered via twemoji)
        #[arg(long, value_name = "EMOJI", conflicts_with_all = ["overlay_svg", "overlay_svg_file", "overlay_emoji_name"])]
        overlay_emoji: Option<String>,

        /// Emoji name for overlay (e.g., "duck", rendered via twemoji)
        #[arg(long, value_name = "NAME", conflicts_with_all = ["overlay_svg", "overlay_svg_file", "overlay_emoji"])]
        overlay_emoji_name: Option<String>,

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
            decal_svg,
            decal_svg_file,
            decal_scale,
            overlay_svg,
            overlay_svg_file,
            overlay_emoji,
            overlay_emoji_name,
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
                if decal_svg.is_some() || decal_svg_file.is_some() {
                    let svg = if let Some(svg) = decal_svg {
                        svg
                    } else if let Some(path) = decal_svg_file {
                        std::fs::read_to_string(&path)
                            .with_context(|| format!("Failed to read decal SVG file: {}", path.display()))?
                    } else {
                        unreachable!()
                    };

                    p = p.with_decal(DecalSettings {
                        source: SerializableSvgSource::from_svg(svg),
                        scale: decal_scale,
                        enabled: true,
                    });
                }

                // Overlay
                if overlay_svg.is_some() || overlay_svg_file.is_some() || overlay_emoji.is_some() || overlay_emoji_name.is_some() {
                    let source = if let Some(svg) = overlay_svg {
                        SerializableSvgSource::from_svg(svg)
                    } else if let Some(path) = overlay_svg_file {
                        let svg = std::fs::read_to_string(&path)
                            .with_context(|| format!("Failed to read overlay SVG file: {}", path.display()))?;
                        SerializableSvgSource::from_svg(svg)
                    } else if let Some(emoji) = overlay_emoji {
                        SerializableSvgSource::from_emoji(emoji)
                    } else if let Some(name) = overlay_emoji_name {
                        SerializableSvgSource::from_emoji_name(name)
                    } else {
                        SerializableSvgSource::default()
                    };

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
