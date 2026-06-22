use gencode as lib;

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use std::fs;

use lib::codec::{encode_bytes, encode_string, decode_to_bytes, decode_to_string, EncodeOptions, DecodeOptions};
use lib::index::IndexTable;
use lib::package::{is_installed, install, init_project};

#[derive(Parser)]
#[command(
    name = "g",
    about = "G — next-generation data encoding system",
    version = env!("CARGO_PKG_VERSION"),
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Encode a file or G string
    Encode {
        #[arg(conflicts_with = "string")]
        file: Option<PathBuf>,

        /// Encode a G-notation string directly
        #[arg(short = 'S', long, conflicts_with = "file")]
        string: Option<String>,

        /// Output file (default: <input>.g)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Gn instance — block size and state count
        #[arg(short, long, default_value = "28")]
        n: usize,

        /// Disable entropy layer (zstd on index stream)
        #[arg(long)]
        no_entropy: bool,

        /// Disable cross-block context
        #[arg(long)]
        no_context: bool,

        /// Disable adaptive Gn selection per chunk
        #[arg(long)]
        no_adaptive: bool,

        /// Disable LZ77 preprocessing
        #[arg(long)]
        no_lz77: bool,


        /// Auto-repair invalid blocks (default: on)
        #[arg(long)]
        strict: bool,

        /// Suppress progress bar
        #[arg(long)]
        quiet: bool,

        /// Print detailed stats after encoding
        #[arg(long)]
        stats: bool,
    },

    /// Decode a .g file
    Decode {
        file: PathBuf,

        /// Output file (default: <input> without .g)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output as G-notation string
        #[arg(short = 'S', long)]
        string: bool,

        /// Override Gn instance (0 = read from file)
        #[arg(short, long, default_value = "0")]
        n: usize,

        /// Keep null characters in output
        #[arg(long)]
        keep_nulls: bool,

        /// Suppress progress bar
        #[arg(long)]
        quiet: bool,

        /// Print detailed stats after decoding
        #[arg(long)]
        stats: bool,
    },

    /// Show information about a Gn instance
    Info {
        #[arg(short, long, default_value = "28")]
        n: usize,

        /// Show adjacency matrix
        #[arg(long)]
        adjacency: bool,

        /// Compare multiple instances
        #[arg(long)]
        compare: bool,
    },

    /// Encode to G canonical text format (.gt)
    EncodeText {
        /// Input file (or - for stdin)
        file: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long, default_value = "28")]
        n: usize,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        stats: bool,
    },

    /// Decode G canonical text format (.gt) back to bytes
    DecodeText {
        /// Input .gt file (or - for stdin)
        file: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        stats: bool,
    },

    /// Validate a .gt file — checks header, charset, adjacency rules
    Validate {
        /// Input .gt file (or - for stdin)
        file: Option<PathBuf>,
    },

    /// Initialize G in a project directory
    Init {
        dir: Option<PathBuf>,
    },

    /// (Re)install G system library
    Install,

    /// Build and install libg (C/Python library) from source — requires cargo
    BuildLib,
}

fn main() {
    if !is_installed() {
        if let Err(e) = install() {
            eprintln!("{} {}", "error:".red().bold(), e);
            std::process::exit(1);
        }
    }

    let cli = Cli::parse();

    let result = match cli.command {
        None => { print_banner(); Ok(()) }
        Some(Command::Encode { file, string, output, n, no_entropy, no_context, no_adaptive, no_lz77, strict, quiet, stats }) =>
            cmd_encode(file, string, output, n, !no_entropy, !no_context, !no_adaptive, !no_lz77, !strict, quiet, stats),
        Some(Command::Decode { file, output, string, n, keep_nulls, quiet, stats }) =>
            cmd_decode(file, output, string, n, keep_nulls, quiet, stats),
        Some(Command::Info { n, adjacency, compare }) =>
            cmd_info(n, adjacency, compare),
        Some(Command::EncodeText { file, output, n, quiet, stats }) =>
            cmd_encode_text(file, output, n, quiet, stats),
        Some(Command::DecodeText { file, output, quiet, stats }) =>
            cmd_decode_text(file, output, quiet, stats),
        Some(Command::Validate { file }) => cmd_validate(file),
        Some(Command::Init { dir }) => cmd_init(dir),
        Some(Command::Install) => install(),
        Some(Command::BuildLib) => cmd_build_lib(),
    };

    if let Err(e) = result {
        eprintln!("{} {}", "error:".red().bold(), e);
        std::process::exit(1);
    }
}

fn cmd_encode(
    file: Option<PathBuf>,
    string: Option<String>,
    output: Option<PathBuf>,
    n: usize,
    entropy: bool,
    context: bool,
    adaptive: bool,
    lz77: bool,
    repair: bool,
    quiet: bool,
    stats: bool,
) -> Result<(), String> {
    let opts = EncodeOptions { n, show_progress: !quiet, repair_invalid: repair, entropy, context, adaptive, lz77 };

    let active: Vec<&str> = [
        if lz77     { Some("lz77") }     else { None },
        if entropy  { Some("entropy") }  else { None },
        if context  { Some("context") }  else { None },
        if adaptive { Some("adaptive") } else { None },
    ].iter().flatten().copied().collect();

    if !quiet {
        println!();
        println!("  {} G{}  [{}]",
            "encoding".cyan().bold(),
            n,
            if active.is_empty() { "raw".to_string() } else { active.join(" + ") },
        );
    }

    let (result, input_name) = if let Some(s) = string {
        let name = format!("\"{}\"", if s.len() > 24 { &s[..24] } else { &s });
        (encode_string(&s, &opts)?, name)
    } else if let Some(ref f) = file {
        let data = fs::read(f).map_err(|e| format!("Cannot read {}: {}", f.display(), e))?;
        (encode_bytes(&data, &opts)?, f.display().to_string())
    } else {
        return Err("Provide a file or --string".to_string());
    };

    let out_path = output.unwrap_or_else(|| {
        file.as_ref().map(|f| f.with_extension("g")).unwrap_or_else(|| PathBuf::from("output.g"))
    });
    fs::write(&out_path, &result.data)
        .map_err(|e| format!("Cannot write {}: {}", out_path.display(), e))?;

    let ratio = result.output_bytes as f64 / result.input_bytes.max(1) as f64;
    let savings_pct = (1.0 - ratio) * 100.0;

    println!();
    println!("  {}  {}  →  {}", "done".green().bold(), input_name.dimmed(), out_path.display().to_string().cyan());
    println!("  {:<18} {:.2} KB  →  {:.2} KB", "size:".bold(),
        result.input_bytes as f64 / 1024.0, result.output_bytes as f64 / 1024.0);
    println!("  {:<18} {:.4}  ({}{:.1}% vs original)", "ratio:".bold(),
        ratio, if savings_pct >= 0.0 { "-" } else { "+" }, (savings_pct as f64).abs());

    if stats {
        println!();
        println!("  {:<18} {}", "blocks:".bold(), format_big(result.blocks as u128));
        println!("  {:<18} {}", "chunks:".bold(), result.chunks);
        if entropy {
            println!("  {:<18} {:.2} KB saved", "entropy layer:".bold(),
                result.entropy_savings as f64 / 1024.0);
        }
        if context {
            println!("  {:<18} ~{} bits saved", "context layer:".bold(), result.context_savings);
        }
        if result.repairs > 0 {
            println!("  {:<18} {} blocks", "auto-repaired:".bold(), result.repairs);
        }
    }
    println!();

    Ok(())
}

fn cmd_decode(
    file: PathBuf,
    output: Option<PathBuf>,
    as_string: bool,
    n: usize,
    keep_nulls: bool,
    quiet: bool,
    stats: bool,
) -> Result<(), String> {
    let data = fs::read(&file).map_err(|e| format!("Cannot read {}: {}", file.display(), e))?;
    let input_size = data.len();
    let opts = DecodeOptions { n, show_progress: !quiet, skip_nulls: !keep_nulls };

    if as_string {
        let s = decode_to_string(&data, &opts)?;
        println!("{}", s);
        return Ok(());
    }

    let result = decode_to_bytes(&data, &opts)?;
    let out_path = output.unwrap_or_else(|| PathBuf::from(file.file_stem().unwrap_or_default()));
    let output_size = result.data.len();
    fs::write(&out_path, &result.data)
        .map_err(|e| format!("Cannot write {}: {}", out_path.display(), e))?;

    println!();
    println!("  {}  {}  →  {}", "done".green().bold(),
        file.display().to_string().dimmed(), out_path.display().to_string().cyan());
    println!("  {:<18} {:.2} KB  →  {:.2} KB", "size:".bold(),
        input_size as f64 / 1024.0, output_size as f64 / 1024.0);

    if stats {
        println!();
        println!("  {:<18} {}", "blocks:".bold(), format_big(result.blocks as u128));
        println!("  {:<18} {}", "nulls discarded:".bold(), format_big(result.nulls_skipped as u128));
    }
    println!();

    Ok(())
}

fn cmd_info(n: usize, show_adjacency: bool, compare: bool) -> Result<(), String> {
    if compare {
        println!();
        println!("  {}", "Gn comparison".bold());
        println!();
        println!("  {:>6}  {:>10}  {:>8}  {:>10}  {:>12}  {:>10}",
            "n", "charset", "bits", "valid", "bits/char", "savings");
        println!("  {}", "─".repeat(64).dimmed());
        for &cn in &[2usize, 4, 8, 12, 16, 20, 24, 28, 32] {
            let t = IndexTable::new(cn);
            let flat = cn * (t.charset.len() as f64).log2().ceil() as usize;
            let savings = flat.saturating_sub(t.bits as usize);
            println!("  {:>6}  {:>10}  {:>8}  {:>10}  {:>12.4}  {:>10}",
                format!("G{}", cn).cyan(),
                t.charset.len(),
                t.bits,
                format_big(t.valid_count),
                t.bits as f64 / cn as f64,
                format!("{} bits", savings),
            );
        }
        println!();
        return Ok(());
    }

    let table = IndexTable::new(n);
    let cs = &table.charset;
    let flat_bits = n * (cs.len() as f64).log2().ceil() as usize;
    let savings = flat_bits.saturating_sub(table.bits as usize);

    println!();
    println!("  {}", format!("G{} instance", n).cyan().bold());
    println!();
    println!("  {:<28} {}", "Block size (chars):".bold(), n);
    println!("  {:<28} {}", "States per character:".bold(), n);
    println!("  {:<28} {}", "Charset size:".bold(), cs.len());
    println!("  {:<28} {}", "Valid blocks V(n):".bold(), format_big(table.valid_count));
    println!("  {:<28} {} bits", "Index width bits(n):".bold(), table.bits);
    println!("  {:<28} {:.4} bits/char", "Information density:".bold(), table.bits as f64 / n as f64);
    println!("  {:<28} {} bits  ({} saved/block)", "Flat encoding:".bold(), flat_bits, savings);
    println!();
    println!("  {}", "Character set:".bold());
    let chars: Vec<String> = cs.iter().map(|g| g.ch.to_string()).collect();
    for chunk in chars.chunks(20) {
        println!("  {}", chunk.join("  ").dimmed());
    }
    println!();

    if show_adjacency {
        use lib::adjacency::can_adjoin;
        println!("  {}", "Adjacency matrix:".bold());
        let cats = ["null", "odd", "even", "lower", "capital", "symbol"];
        let ex   = ['0',   '1',  '2',   'a',   'A',     '!'];
        print!("  {:>10}", "");
        for c in &cats { print!("  {:>8}", c); }
        println!();
        for (i, &a) in ex.iter().enumerate() {
            print!("  {:>10}", cats[i].bold().to_string());
            for &b in &ex {
                print!("  {:>8}", if can_adjoin(a, b) { "✓".green() } else { "✗".red() }.to_string());
            }
            println!();
        }
        println!();
    }

    Ok(())
}

fn cmd_init(dir: Option<PathBuf>) -> Result<(), String> {
    let project_dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    init_project(&project_dir)
}

fn print_banner() {
    println!();
    println!("  {}  next-generation data encoding", "G".cyan().bold());
    println!();
    println!("  {}", "COMMANDS".bold());
    println!("    {}   encode a file", "g encode <file>".bold());
    println!("    {}    decode a .g file", "g decode <file>".bold());
    println!("    {}     show instance info", "g info [-n N]".bold());
    println!("    {}    compare all instances", "g info --compare".bold());
    println!("    {}  encode to .gt text format", "g encode-text <file>".bold());
    println!("    {}  decode .gt text format", "g decode-text <file>".bold());
    println!("    {}       validate a .gt file", "g validate <file>".bold());
    println!("    {}     init G in a project", "g init".bold());
    println!("    {}       build and install libg from source", "g build-lib".bold());
    println!();
    println!("  {}", "ENCODE FLAGS".bold());
    println!("    {}              Gn instance (default: 28)", "-n <N>".bold());
    println!("    {}        disable entropy layer (zstd)", "--no-entropy".bold());
    println!("    {}        disable cross-block context", "--no-context".bold());
    println!("    {}       disable adaptive Gn per chunk", "--no-adaptive".bold());
    println!("    {}            disable LZ77 preprocessing", "--no-lz77".bold());
    println!("    {}              error on invalid blocks", "--strict".bold());
    println!("    {}               suppress progress bar", "--quiet".bold());
    println!("    {}               show detailed stats", "--stats".bold());
    println!();
    println!("  Run {} for full options", "g <command> --help".bold());
    println!();
}

fn format_big(n: u128) -> String {
    if n == u128::MAX { return "overflow".to_string(); }
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { result.push(','); }
        result.push(c);
    }
    result.chars().rev().collect()
}

// ── Text format commands ────────────────────────────────────────────────────

fn cmd_encode_text(
    file: Option<PathBuf>,
    output: Option<PathBuf>,
    n: usize,
    quiet: bool,
    stats: bool,
) -> Result<(), String> {
    let (data, input_name) = read_input_data(file.as_ref())?;
    let text = lib::text::encode_to_text(&data, n)?;

    let out_path = output.unwrap_or_else(|| {
        file.as_ref()
            .map(|f| f.with_extension("gt"))
            .unwrap_or_else(|| PathBuf::from("output.gt"))
    });

    fs::write(&out_path, &text)
        .map_err(|e| format!("Cannot write {}: {}", out_path.display(), e))?;

    if !quiet {
        let lines = text.lines().count().saturating_sub(1); // minus header
        println!();
        println!("  {}  {}  →  {}", "encoded".green().bold(),
            input_name.dimmed(), out_path.display().to_string().cyan());
        println!("  {:<18} {} bytes  →  {} bytes", "size:".bold(), data.len(), text.len());
        if stats {
            println!("  {:<18} Gn/0/1/0/{}/{}", "header:".bold(), n, data.len());
            println!("  {:<18} {}", "blocks:".bold(), lines);
        }
        println!();
    }
    Ok(())
}

fn cmd_decode_text(
    file: Option<PathBuf>,
    output: Option<PathBuf>,
    quiet: bool,
    stats: bool,
) -> Result<(), String> {
    let (raw, input_name) = read_input_data(file.as_ref())?;
    let text = String::from_utf8(raw).map_err(|e| format!("Not valid UTF-8: {}", e))?;
    let data = lib::text::decode_from_text(&text)?;

    let out_path = output.unwrap_or_else(|| {
        file.as_ref()
            .and_then(|f| if f.extension().map(|e| e == "gt").unwrap_or(false) {
                Some(PathBuf::from(f.file_stem().unwrap_or_default()))
            } else {
                Some(f.with_extension("decoded"))
            })
            .unwrap_or_else(|| PathBuf::from("output"))
    });

    fs::write(&out_path, &data)
        .map_err(|e| format!("Cannot write {}: {}", out_path.display(), e))?;

    if !quiet {
        println!();
        println!("  {}  {}  →  {}", "decoded".green().bold(),
            input_name.dimmed(), out_path.display().to_string().cyan());
        println!("  {:<18} {} bytes  →  {} bytes", "size:".bold(), text.len(), data.len());
        if stats {
            println!("  {:<18} {}", "output bytes:".bold(), data.len());
        }
        println!();
    }
    Ok(())
}

fn cmd_validate(file: Option<PathBuf>) -> Result<(), String> {
    let (raw, input_name) = read_input_data(file.as_ref())?;
    let text = String::from_utf8(raw).map_err(|e| format!("Not valid UTF-8: {}", e))?;
    let result = lib::text::validate_text(&text);

    if result.valid {
        let h = result.header.unwrap();
        println!();
        println!("  {} {}", "✓".green().bold(), input_name.cyan());
        println!("  {:<18} Gn/{}/{}/{}", "version:".bold(), h.major, h.minor, h.patch);
        println!("  {:<18} G{}", "instance:".bold(), h.n);
        println!("  {:<18} {}", "blocks:".bold(), result.block_count);
        println!("  {:<18} {} bytes", "original size:".bold(), h.original_bytes);
        println!();
        Ok(())
    } else {
        eprintln!();
        eprintln!("  {} {}", "✗".red().bold(), input_name);
        for e in &result.errors {
            eprintln!("  {} {}", "·".red(), e);
        }
        eprintln!();
        Err("validation failed".to_string())
    }
}

/// Read from file path, or stdin if path is None or "-"
fn read_input_data(path: Option<&PathBuf>) -> Result<(Vec<u8>, String), String> {
    let use_stdin = match path {
        None => true,
        Some(p) => p.to_str() == Some("-"),
    };
    if use_stdin {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).map_err(|e| format!("stdin read error: {}", e))?;
        Ok((buf, "<stdin>".to_string()))
    } else {
        let p = path.unwrap();
        let data = fs::read(p).map_err(|e| format!("Cannot read {}: {}", p.display(), e))?;
        Ok((data, p.display().to_string()))
    }
}

fn cmd_build_lib() -> Result<(), String> {
    lib::package::build_lib()
}
