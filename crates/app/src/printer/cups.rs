//! Printing on Linux through CUPS's command-line tools (decision 11). With no `lp` on the
//! path *Print* is disabled with the reason, and *Save as PDF* still works.

use std::io::BufWriter;
use std::process::{Child, ChildStdin, Command, Stdio};

use super::{Capabilities, Printer};
use crate::pages::Page;
use crate::pdf::Pdf;
use crate::print::Paper;

/// *Printer properties…* is the driver's own dialog on Windows; CUPS has none to open.
pub const HAS_PROPERTIES: bool = false;

/// Why printing is not possible here, or `None` when it is.
pub fn unavailable() -> Option<String> {
    let found = Command::new("lp")
        .arg("-?")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match found {
        Err(_) => Some("CUPS's `lp` is not installed. Save as PDF and print that.".into()),
        Ok(_) => None,
    }
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The printers CUPS knows, the default marked.
pub fn list() -> Vec<Printer> {
    let default = run("lpstat", &["-d"])
        .and_then(|s| {
            s.split_once(':')
                .map(|(_, name)| name.trim().to_owned())
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_default();
    run("lpstat", &["-e"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|name| Printer {
            name: name.to_owned(),
            is_default: name == default,
        })
        .collect()
}

/// CUPS's names for each paper, the first the one `lp -o media=` is given.
fn names(paper: Paper) -> &'static [&'static str] {
    match paper {
        Paper::Letter => &["Letter"],
        Paper::Legal => &["Legal"],
        Paper::Tabloid => &["Tabloid", "11x17", "Ledger"],
        Paper::A5 => &["A5"],
        Paper::A4 => &["A4"],
        Paper::A3 => &["A3"],
    }
}

/// The papers in a `PageSize/Media Size: a *b c` line's choices, and the name to use for each.
fn papers(options: &str) -> Vec<(Paper, String)> {
    let Some(line) = options
        .lines()
        .find(|l| l.starts_with("PageSize/") || l.starts_with("PageSize:"))
    else {
        return Vec::new();
    };
    let choices: Vec<&str> = line
        .split_once(':')
        .map(|(_, c)| c)
        .unwrap_or_default()
        .split_whitespace()
        .map(|c| c.trim_start_matches('*'))
        .collect();
    Paper::ALL
        .into_iter()
        .filter_map(|p| {
            names(p)
                .iter()
                .find(|n| choices.contains(n))
                .map(|n| (p, (*n).to_owned()))
        })
        .collect()
}

pub fn capabilities(printer: &str) -> Capabilities {
    let options = run("lpoptions", &["-p", printer, "-l"]).unwrap_or_default();
    Capabilities {
        papers: papers(&options).into_iter().map(|(p, _)| p).collect(),
        dpi: None,
        margin_pt: None,
    }
}

/// Opens the driver's own dialog: CUPS has none.
pub fn properties(_printer: &str) -> Result<(), String> {
    Ok(())
}

/// What to print and how.
pub struct Request {
    pub printer: String,
    pub paper: Paper,
    /// Unused: a landscape page is a wide PDF page, which CUPS turns by itself.
    #[allow(dead_code)]
    pub landscape: bool,
    pub copies: u32,
    pub title: String,
}

/// A job being sent: a PDF written to `lp`'s stdin a page at a time.
pub struct Spool {
    pdf: Pdf<BufWriter<ChildStdin>>,
    child: Child,
    printer: String,
}

impl Spool {
    pub fn open(r: &Request) -> Result<Self, String> {
        Self::open_with("lp", r)
    }

    fn open_with(program: &str, r: &Request) -> Result<Self, String> {
        let options = run("lpoptions", &["-p", &r.printer, "-l"]).unwrap_or_default();
        let media = papers(&options)
            .into_iter()
            .find(|(p, _)| *p == r.paper)
            .map(|(_, n)| n)
            .unwrap_or_else(|| names(r.paper)[0].to_owned());
        let mut child = Command::new(program)
            .args(["-d", &r.printer, "-n", &r.copies.max(1).to_string()])
            .args(["-o", &format!("media={media}"), "-o", "print-scaling=none"])
            .args(["-t", &r.title, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not run lp: {e}"))?;
        let stdin = child.stdin.take().ok_or("lp has no input")?;
        let pdf = Pdf::new(BufWriter::new(stdin), &r.title).map_err(|e| e.to_string())?;
        Ok(Self {
            pdf,
            child,
            printer: r.printer.clone(),
        })
    }

    pub fn page(&mut self, page: &Page) -> Result<(), String> {
        match self.pdf.page(&page.rgb, page.size, page.size_pt) {
            Ok(()) => Ok(()),
            Err(e) => Err(failure(&mut self.child, &e.to_string())),
        }
    }

    /// Closes the document and waits for CUPS to accept it.
    pub fn finish(self) -> Result<String, String> {
        let Self {
            pdf,
            mut child,
            printer,
        } = self;
        match pdf.finish() {
            Ok(w) => drop(w),
            Err(e) => return Err(failure(&mut child, &e.to_string())),
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(format!("Sent to {printer}."))
        } else {
            Err(format!(
                "lp: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// What `lp` said when it refused the job, else `fallback`.
fn failure(child: &mut Child, fallback: &str) -> String {
    use std::io::Read;
    let _ = child.kill();
    let mut said = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut said);
    }
    let _ = child.wait();
    let said = if said.trim().is_empty() {
        fallback
    } else {
        said.trim()
    };
    format!("lp: {said}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_job_is_a_pdf_on_lps_stdin() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let lp = dir.path().join("lp");
        std::fs::write(
            &lp,
            format!(
                "#!/bin/sh\necho \"$@\" > {0}/args\ncat > {0}/in.pdf\n",
                dir.path().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&lp, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = Request {
            printer: "Test".into(),
            paper: Paper::A4,
            landscape: false,
            copies: 3,
            title: "Trip".into(),
        };
        let mut spool = Spool::open_with(lp.to_str().unwrap(), &r).unwrap();
        let page = Page {
            size: [2, 2],
            size_pt: [595.0, 842.0],
            rgb: vec![255; 12],
        };
        spool.page(&page).unwrap();
        assert_eq!(spool.finish().unwrap(), "Sent to Test.");
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert_eq!(
            args.trim(),
            "-d Test -n 3 -o media=A4 -o print-scaling=none -t Trip -"
        );
        let pdf = std::fs::read(dir.path().join("in.pdf")).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn papers_come_from_the_page_size_line() {
        let o = "PageSize/Media Size: A4 A4.Borderless *Letter Letter.Borderless Legal 11x17\nColorModel/Color: *RGB Gray\n";
        let p = papers(o);
        assert_eq!(
            p,
            [
                (Paper::Letter, "Letter".to_string()),
                (Paper::Legal, "Legal".to_string()),
                (Paper::Tabloid, "11x17".to_string()),
                (Paper::A4, "A4".to_string()),
            ]
        );
        assert!(papers("ColorModel/Color: *RGB\n").is_empty());
    }
}
