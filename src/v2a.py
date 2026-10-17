p='crisol-core/src/nginx.rs'
s=open(p,encoding='utf-8').read()
def rep(a,b,n=1):
    global s
    assert a in s, a
    s=s.replace(a,b,n)

rep("""    /// Sites left out and other things worth saying.
    pub warnings: Vec<String>,
}

/// Why the configuration could not be written.""","""    /// Sites left out and other things worth saying.
    pub warnings: Vec<String>,
    /// Identifies this configuration: it is part of the text of the default server as
    /// `/.crisol-config-<token>` (a path only the computer itself may ask for), so that asking
    /// for it tells whether a running Nginx has already loaded this configuration.
    pub token: String,
}

/// The path that answers `204` to this computer once Nginx runs the configuration with `token`.
#[must_use]
pub fn probe_path(token: &str) -> String {
    format!("/.crisol-config-{token}")
}

/// Why the configuration could not be written.""")
rep("""        Ok(etc) => render_in(input, &etc),
        Err(e) => RenderedNginx {
            files: BTreeMap::new(),
            warnings: vec![e.to_string()],
        },
    }
}
""","""        Ok(etc) => {
            // The token is the hash of everything else, so it changes with every change of the
            // configuration and with nothing else.
            let bare = render_in(input, &etc, "");
            let token = token_of(&bare.files);
            render_in(input, &etc, &token)
        }
        Err(e) => RenderedNginx {
            files: BTreeMap::new(),
            warnings: vec![e.to_string()],
            token: String::new(),
        },
    }
}

fn token_of(files: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (name, text) in files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(text.as_bytes());
        hasher.update([0]);
    }
    hasher.finalize().iter().take(8).fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
""")
rep("fn render_in(input: &NginxInput<'_>, etc: &Path) -> RenderedNginx {","fn render_in(input: &NginxInput<'_>, etc: &Path, token: &str) -> RenderedNginx {")
rep("        render_main(input, etc, any_secure),","        render_main(input, etc, any_secure, token),")
rep("    RenderedNginx { files, warnings }\n}","    RenderedNginx {\n        files,\n        warnings,\n        token: token.to_owned(),\n    }\n}")
rep("fn render_main(input: &NginxInput<'_>, etc: &Path, any_secure: bool) -> Result<String, String> {","fn render_main(\n    input: &NginxInput<'_>,\n    etc: &Path,\n    any_secure: bool,\n    token: &str,\n) -> Result<String, String> {")
rep(r"""         \x20       server_name _;\n\
         \x20       return 444;\n\
         \x20   }}\n\n\
         {tls_default}\""",r"""         \x20       server_name _;\n\
         {probe}\
         \x20       location / {{\n\
         \x20           return 444;\n\
         \x20       }}\n\
         \x20   }}\n\n\
         {tls_default}\""")
rep("""    let mut text = String::from(HEADER);
    let _ = write!(
        text,
        "# User settings""","""    // Only this computer may ask: whoever else gets the same closed connection as for any
    // unknown name.
    let probe = if token.is_empty() {
        String::new()
    } else {
        format!(
            "        # Whether Nginx runs this configuration yet (the name changes with it).\n\
             \x20       location = {} {{\n\
             \x20           allow 127.0.0.1;\n\
             \x20           allow ::1;\n\
             \x20           deny all;\n\
             \x20           return 204;\n\
             \x20       }}\n",
            probe_path(token)
        )
    };

    let mut text = String::from(HEADER);
    let _ = write!(
        text,
        "# User settings""")
rep("    let rendered = render_in(input, &stage);","    let real = render(input);\n    let rendered = RenderedNginx {\n        files: render_in(input, &stage, &real.token).files,\n        ..real\n    };")
rep("use crate::paths::{require_private_home","use sha2::{Digest, Sha256};\n\nuse crate::paths::{require_private_home")
open(p,'w',encoding='utf-8').write(s)
