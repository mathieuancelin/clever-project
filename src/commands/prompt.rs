//! Tiny stdin/stdout prompt helpers. Kept generic over `BufRead`/`Write` so
//! the interactive flow can be unit-tested without touching the real
//! terminal.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::Result;

/// `true` if stdin is connected to a terminal. CI/piped invocations get
/// `false` and the caller should either auto-approve or fail loudly.
pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

/// Prompt the user with a yes/no question. `default` is returned on empty
/// input or EOF (so piping `echo ""` selects the default). Bogus answers
/// re-prompt.
pub fn ask_yes_no<R: BufRead, W: Write>(
    prompt: &str,
    default: bool,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        write!(stdout, "{prompt}? [{hint}]: ")?;
        stdout.flush()?;
        let mut line = String::new();
        let n = stdin.read_line(&mut line)?;
        if n == 0 {
            return Ok(default);
        }
        let s = line.trim().to_lowercase();
        if s.is_empty() {
            return Ok(default);
        }
        match s.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => writeln!(stdout, "  ! please answer y or n")?,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn yes_returns_true() {
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(ask_yes_no("Continue", false, &mut input, &mut output).unwrap());
    }

    #[test]
    fn no_returns_false() {
        let mut input = Cursor::new(b"n\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(!ask_yes_no("Continue", true, &mut input, &mut output).unwrap());
    }

    #[test]
    fn empty_returns_default() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(!ask_yes_no("Continue", false, &mut input, &mut output).unwrap());
        let mut input2 = Cursor::new(b"\n".to_vec());
        let mut output2: Vec<u8> = Vec::new();
        assert!(ask_yes_no("Continue", true, &mut input2, &mut output2).unwrap());
    }

    #[test]
    fn eof_returns_default() {
        let mut input = Cursor::new(Vec::new());
        let mut output: Vec<u8> = Vec::new();
        assert!(!ask_yes_no("Continue", false, &mut input, &mut output).unwrap());
    }

    #[test]
    fn retries_on_garbage() {
        let mut input = Cursor::new(b"maybe\nidk\ny\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(ask_yes_no("Continue", false, &mut input, &mut output).unwrap());
        let s = String::from_utf8(output).unwrap();
        // Two error lines printed before the final success.
        assert_eq!(s.matches("please answer y or n").count(), 2);
    }

    #[test]
    fn yes_long_form_accepted() {
        let mut input = Cursor::new(b"YES\n".to_vec());
        let mut output: Vec<u8> = Vec::new();
        assert!(ask_yes_no("Continue", false, &mut input, &mut output).unwrap());
    }
}
