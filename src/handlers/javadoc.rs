/* SPDX-License-Identifier: GPL-3.0-or-later */

use anyhow::Result;
use log::{debug, info};
use regex::{Regex, RegexBuilder};
use std::io;
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;
use std::rc::Rc;

use crate::handlers::InputOutputHelper;
use crate::options;

const HEADER_LINES_TO_CHECK: i32 = 15;

pub struct Javadoc {
    config: Rc<options::Config>,
}

impl Javadoc {
    pub fn boxed(config: &Rc<options::Config>) -> Box<dyn super::Processor> {
        Box::new(Self { config: config.clone() })
    }

    fn process_line(&self, line: &str) -> Result<Option<String>> {
        // javadoc files have the date in two places in the header:
        //   <!-- Generated by javadoc (21) on Sat Mar 02 16:07:41 UTC 2024 -->
        //   <meta name="dc.created" content="2024-03-02">
        //
        // We strip the javadoc version and date in the first line, based on the
        // assumption that this is just a freeform comment and the date is not
        // parsed by anything. The information that this was generated by javadoc is
        // retained to that is useful information (and because people sometimes
        // modify generated files by hand, wasting their time).
        //
        // In the second line, we parse the date as %Y-%m-%d, compare is with
        // $SOURCE_DATE_EPOCH, and replace if newer. This means that we'll not
        // rewrite this line in pages that were generated a long time ago.

        let re = Regex::new(r"(.*<!-- Generated by javadoc) .+ (-->.*)")?;
        if let Some(caps) = re.captures(line) {
            return Ok(Some(format!("{} {}", &caps[1], &caps[2])));
        }

        let epoch = self.config.source_date_epoch
            .map(|v| chrono::DateTime::from_timestamp(v, 0).unwrap());

        if let Some(epoch) = epoch {
            let re = RegexBuilder::new(r#"<(meta name="(date|dc\.created)" content=)"([^"]+)">"#)
                .case_insensitive(true)
                .build()?;

            if let Some(caps) = re.captures(line) {
                match chrono::NaiveDate::parse_from_str(&caps[3], "%Y-%m-%d") {
                    Err(_) => {
                        debug!("Failed to parse naive date: {:?}", &caps[3]);
                    }
                    Ok(date) => {
                        debug!("Matched meta {} date {} → {:?}", &caps[2], &caps[3], date);
                        if epoch.date_naive() < date {
                            let ts = epoch.format("%Y-%m-%d");
                            return Ok(Some(format!("<{}\"{}\">", &caps[1], ts)));
                        }
                    }
                }
            }
        }

        Ok(None)
    }
}

impl super::Processor for Javadoc {
    fn name(&self) -> &str {
        "javadoc"
    }

    fn filter(&self, path: &Path) -> Result<bool> {
        Ok(path.extension().is_some_and(|x| x == "html")
           // && path.to_str().is_some_and(|x| x.contains("/usr/share/javadoc/"))
        )
    }

    fn process(&self, input_path: &Path) -> Result<super::ProcessResult> {
        let mut have_mod = false;
        let mut after_header = false;

        let (mut io, input) = InputOutputHelper::open(input_path, self.config.check, true)?;

        io.open_output()?;
        let mut output = BufWriter::new(io.output.as_mut().unwrap());

        let head_end_re = RegexBuilder::new(r"</head>")
            .case_insensitive(true)
            .build()?;

        let mut num = 0;
        for line in input.lines() {
            let line = match line {
                Err(e) => {
                    if e.kind() == io::ErrorKind::InvalidData {
                        info!("{}:{}: {}, ignoring.", input_path.display(), num + 1, e);
                        return Ok(super::ProcessResult::Noop);
                    } else {
                        return Err(e.into());
                    }
                }
                Ok(line) => line
            };

            num += 1;

            let line2 = if !after_header { self.process_line(&line)? } else { None };

            if line2.is_some() && !have_mod {
                debug!("{}:{}: found first line to replace: {:?}", input_path.display(), num, line);
                have_mod = true;
            }

            if !after_header && (num >= HEADER_LINES_TO_CHECK || head_end_re.find(&line).is_some()) {
                if !have_mod {
                    let why = if num >= HEADER_LINES_TO_CHECK {
                        format!("first {HEADER_LINES_TO_CHECK} lines")
                    } else {
                        String::from("until header end")
                    };

                    debug!("{}:{}: found nothing to replace {}", input_path.display(), num, why);
                    return Ok(super::ProcessResult::Noop);
                }

                after_header = true;
            }

            writeln!(output, "{}", line2.unwrap_or(line))?;
        }

        output.flush()?;
        drop(output);

        io.finalize(have_mod)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_html() {
        let cfg = Rc::new(options::Config::empty(1704106800, false));
        let h = Javadoc::boxed(&cfg);

        assert!( h.filter(Path::new("/some/path/page.html")).unwrap());
        assert!(!h.filter(Path::new("/some/path/page.htmll")).unwrap());
        assert!(!h.filter(Path::new("/some/path/page.html.jpg")).unwrap());
        assert!(!h.filter(Path::new("/some/path/page")).unwrap());
        assert!(!h.filter(Path::new("/some/path/html")).unwrap());
        assert!(!h.filter(Path::new("/some/path/html_html")).unwrap());
        assert!(!h.filter(Path::new("/")).unwrap());
    }

    #[test]
    fn test_process_line() {
        let config = Rc::new(options::Config::empty(1704106800, false));
        let h = Javadoc { config };
        let plu = |s| h.process_line(s).unwrap();

        assert_eq!(plu("<!-- Generated by javadoc (21) on Sat Mar 02 16:07:41 UTC 2024 -->").unwrap(),
                   "<!-- Generated by javadoc -->");

        // If we're running on an already processed file, don't report this as a match
        assert!(plu("<!-- Generated by javadoc -->").is_none());

        assert_eq!(plu(r#"<meta name="dc.created" content="2024-03-02">"#).unwrap(),
                   r#"<meta name="dc.created" content="2024-01-01">"#);

        assert_eq!(plu(r#"<META NAME="dc.created" CONTENT="2024-03-02">"#).unwrap(),
                   r#"<META NAME="dc.created" CONTENT="2024-01-01">"#);

        // Too old
        assert!(plu(r#"<META NAME="dc.created" CONTENT="2023-09-09">"#).is_none());

        // Misformatted
        assert!(plu(r#"<META NAME="dc.created" CONTENT="2025">"#).is_none());
        assert!(plu(r#"<META NAME="dc.created" CONTENT="2025-13-01">"#).is_none());
        assert!(plu(r#"<META NAME="dc.created" CONTENT="2025-01-40">"#).is_none());
    }
}
