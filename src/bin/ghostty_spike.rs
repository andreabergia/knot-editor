use libghostty_vt::{Terminal, TerminalOptions};

const COLUMNS: u16 = 40;
const ROWS: u16 = 8;
const REPLAY_CHUNK_SIZE: usize = 13;

struct Recording {
    name: &'static str,
    bytes: &'static [u8],
}

const RECORDINGS: &[Recording] = &[
    Recording {
        name: "styles-and-unicode",
        bytes: concat!(
            "\x1b[1;34mbold blue\x1b[0m ",
            "\x1b[3;38;2;255;128;0mitalic orange\x1b[0m\r\n",
            "ASCII | e\u{301} | 👩🏽‍💻 | 漢字 | \r\n",
            "\x1b[4;9munderline strike\x1b[0m\r\n",
        )
        .as_bytes(),
    },
    Recording {
        name: "cursor-and-scrollback",
        bytes: concat!(
            "line 00\r\nline 01\r\nline 02\r\nline 03\r\n",
            "line 04\r\nline 05\r\nline 06\r\nline 07\r\n",
            "line 08\r\nline 09\r\nline 10\r\nline 11\r\n",
            "\x1b[2A\x1b[6C\x1b[31mEDIT\x1b[0m",
            "\x1b[2;1H\x1b[2Krewritten row",
        )
        .as_bytes(),
    },
    Recording {
        name: "alternate-screen",
        bytes: concat!(
            "\x1b[?1049h\x1b[2J\x1b[H",
            "\x1b[7m alternate screen \x1b[0m",
            "\x1b[8;1Hstatus: ready",
            "\x1b[?25l\x1b[?25h\x1b[?1049l",
        )
        .as_bytes(),
    },
];

fn main() -> anyhow::Result<()> {
    let mut terminal = Terminal::new(TerminalOptions {
        cols: COLUMNS,
        rows: ROWS,
        max_scrollback: 100,
    })?;

    let mut bytes_replayed = 0;
    for recording in RECORDINGS {
        for chunk in recording.bytes.chunks(REPLAY_CHUNK_SIZE) {
            terminal.vt_write(chunk);
        }
        bytes_replayed += recording.bytes.len();
        println!(
            "replayed {} ({} bytes)",
            recording.name,
            recording.bytes.len()
        );
    }

    println!(
        "fed {bytes_replayed} recorded bytes into a {COLUMNS}x{ROWS} Ghostty terminal without a PTY"
    );
    Ok(())
}
