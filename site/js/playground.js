// Toolpath Interactive Playground
// xterm.js terminal with wasm-compiled path CLI

(function () {
  "use strict";

  // --- Virtual Filesystem ---
  function VirtualFS(files) {
    this.files = files || {};
  }
  VirtualFS.prototype.list = function () {
    return Object.keys(this.files).sort();
  };
  VirtualFS.prototype.get = function (name) {
    return this.files[name] || null;
  };
  VirtualFS.prototype.has = function (name) {
    return name in this.files;
  };
  VirtualFS.prototype.size = function (name) {
    var content = this.files[name];
    if (!content) return 0;
    // Approximate byte size
    var bytes = 0;
    for (var i = 0; i < content.length; i++) {
      var c = content.charCodeAt(i);
      bytes += c < 128 ? 1 : c < 2048 ? 2 : 3;
    }
    return bytes;
  };
  VirtualFS.prototype.formatSize = function (bytes) {
    if (bytes < 1024) return bytes + "B";
    return (bytes / 1024).toFixed(1) + "K";
  };

  // --- Command Parser ---
  function parseCommand(line) {
    var tokens = [];
    var current = "";
    var inQuote = false;
    var quoteChar = "";
    for (var i = 0; i < line.length; i++) {
      var ch = line[i];
      if (inQuote) {
        if (ch === quoteChar) {
          inQuote = false;
        } else {
          current += ch;
        }
      } else if (ch === '"' || ch === "'") {
        inQuote = true;
        quoteChar = ch;
      } else if (ch === " " || ch === "\t") {
        if (current) {
          tokens.push(current);
          current = "";
        }
      } else {
        current += ch;
      }
    }
    if (current) tokens.push(current);
    return tokens;
  }

  // --- ANSI helpers ---
  var ANSI = {
    reset: "\x1b[0m",
    bold: "\x1b[1m",
    dim: "\x1b[2m",
    copper: "\x1b[33m", // yellow slot = copper
    red: "\x1b[31m",
    green: "\x1b[32m",
    pencil: "\x1b[90m", // bright black = pencil
    white: "\x1b[37m",
    cyan: "\x1b[36m",
    magenta: "\x1b[35m",
  };

  function copperBold(s) {
    return ANSI.copper + ANSI.bold + s + ANSI.reset;
  }
  function red(s) {
    return ANSI.red + s + ANSI.reset;
  }
  function dim(s) {
    return ANSI.dim + s + ANSI.reset;
  }
  function pencil(s) {
    return ANSI.pencil + s + ANSI.reset;
  }

  // --- Wasm layer ---
  // Precompiled WebAssembly.Module (compiled once, instantiated per command)
  var compiledWasm = null;
  var wasmFiles = null;
  var wasmReady = false;
  var wasmError = null;

  function loadWasm(files) {
    wasmFiles = files;
    if (typeof createPathModule !== "function") {
      wasmError = "Wasm module not loaded";
      return Promise.reject(new Error(wasmError));
    }
    // Compile once — instantiation per command reuses this
    return fetch("/wasm/path.wasm")
      .then(function (resp) {
        return WebAssembly.compileStreaming(resp);
      })
      .then(function (mod) {
        compiledWasm = mod;
        wasmReady = true;
      })
      .catch(function (err) {
        wasmError = err.message || String(err);
        throw err;
      });
  }

  // Fresh Emscripten instance per call — exit() kills the instance, not us
  function runPath(args) {
    var stdout = "";
    var stderr = "";
    return createPathModule({
      noInitialRun: true,
      instantiateWasm: function (imports, callback) {
        WebAssembly.instantiate(compiledWasm, imports).then(
          function (instance) {
            callback(instance);
          },
        );
        return {};
      },
      print: function (text) {
        stdout += text + "\n";
      },
      printErr: function (text) {
        stderr += text + "\n";
      },
    }).then(function (mod) {
      for (var name in wasmFiles) {
        mod.FS.writeFile("/" + name, wasmFiles[name]);
      }
      try {
        mod.callMain(args);
      } catch (e) {
        // exit() throws to unwind — expected
      }
      return { stdout: stdout.trimEnd(), stderr: stderr.trimEnd() };
    });
  }

  // --- Shell builtins ---
  function cmdHelp(fs) {
    return [
      copperBold("TOOLPATH PLAYGROUND") +
        " " +
        dim("-- real path CLI compiled to WebAssembly"),
      "",
      dim("Shell builtins:"),
      "  " + copperBold("ls") + "                                   List files",
      "  " +
        copperBold("cat") +
        " <file>                           Display file contents",
      "  " +
        copperBold("clear") +
        "                                Clear terminal",
      "  " +
        copperBold("help") +
        "                                 Show this message",
      "",
      dim("CLI commands (run the real binary):"),
      "  " +
        copperBold("path validate") +
        " --input <file>         Validate document",
      "  " +
        copperBold("path query dead-ends") +
        " --input <file>  Find abandoned branches",
      "  " +
        copperBold("path query ancestors") +
        " --input <file> --step-id <id>",
      "                                       Walk parent chain",
      "  " +
        copperBold("path query filter") +
        " --input <file> --actor <prefix>",
      "                                       Filter steps by actor",
      "  " +
        copperBold("path render dot") +
        " --input <file>       Generate DOT graph",
      "  " +
        copperBold("path merge") +
        " <files...>                Merge documents",
      "  " +
        copperBold("path haiku") +
        "                           Random haiku",
      "  " +
        copperBold("path --help") +
        "                          Full CLI usage",
      "",
      dim("Files: " + fs.list().join(", ")),
    ].join("\r\n");
  }

  function cmdLs(fs) {
    var files = fs.list();
    var lines = [];
    for (var i = 0; i < files.length; i++) {
      var size = fs.formatSize(fs.size(files[i]));
      var padded =
        size +
        Array(Math.max(0, 7 - size.length))
          .fill(" ")
          .join("");
      lines.push("  " + pencil(padded) + " " + files[i]);
    }
    return lines.join("\r\n");
  }

  function cmdCat(fs, tokens) {
    var file = tokens[1];
    if (!file) return red("Usage: cat <file>");
    if (!fs.has(file)) return red("cat: " + file + ": No such file");
    return fs.get(file);
  }

  // --- Command dispatcher ---
  function dispatch(line, fs) {
    var tokens = parseCommand(line.trim());
    if (tokens.length === 0) return null;

    var cmd = tokens[0];

    if (cmd === "clear") return { clear: true };
    if (cmd === "help") return { output: cmdHelp(fs) };
    if (cmd === "ls") return { output: cmdLs(fs) };
    if (cmd === "cat") return { output: cmdCat(fs, tokens) };

    if (cmd === "path") {
      if (!wasmReady) {
        if (wasmError) {
          return { output: red("Wasm failed to load: " + wasmError) };
        }
        return { output: dim("Loading CLI... try again in a moment.") };
      }
      var args = tokens.slice(1);
      // Returns a Promise — callers handle this
      return runPath(args)
        .then(function (result) {
          var output = "";
          if (result.stdout) output += result.stdout;
          if (result.stderr) {
            if (output) output += "\r\n";
            output += result.stderr;
          }
          return { output: output || dim("(no output)") };
        })
        .catch(function (err) {
          return { output: red("Error: " + (err.message || err)) };
        });
    }

    return {
      output: red(
        "Unknown command: " +
          cmd +
          ". Type " +
          copperBold("help") +
          " for usage.",
      ),
    };
  }

  // --- Word boundary helpers ---
  function wordBoundaryRight(line, pos) {
    var i = pos;
    // skip current word chars
    while (i < line.length && /\w/.test(line[i])) i++;
    // skip non-word chars
    while (i < line.length && !/\w/.test(line[i])) i++;
    return i;
  }

  function wordBoundaryLeft(line, pos) {
    var i = pos;
    // skip non-word chars behind cursor
    while (i > 0 && !/\w/.test(line[i - 1])) i--;
    // skip word chars
    while (i > 0 && /\w/.test(line[i - 1])) i--;
    return i;
  }

  function wordEndRight(line, pos) {
    var i = pos;
    // skip non-word chars
    while (i < line.length && !/\w/.test(line[i])) i++;
    // skip word chars
    while (i < line.length && /\w/.test(line[i])) i++;
    return i;
  }

  // --- WORD boundary helpers (whitespace-delimited, vim W/B/E) ---
  function WORDBoundaryRight(line, pos) {
    var i = pos;
    // skip current non-whitespace
    while (i < line.length && line[i] !== " " && line[i] !== "\t") i++;
    // skip whitespace
    while (i < line.length && (line[i] === " " || line[i] === "\t")) i++;
    return i;
  }

  function WORDBoundaryLeft(line, pos) {
    var i = pos;
    // skip whitespace behind cursor
    while (i > 0 && (line[i - 1] === " " || line[i - 1] === "\t")) i--;
    // skip non-whitespace
    while (i > 0 && line[i - 1] !== " " && line[i - 1] !== "\t") i--;
    return i;
  }

  function WORDEndRight(line, pos) {
    var i = pos;
    // skip whitespace
    while (i < line.length && (line[i] === " " || line[i] === "\t")) i++;
    // skip non-whitespace
    while (i < line.length && line[i] !== " " && line[i] !== "\t") i++;
    return i;
  }

  // --- Terminal Shell ---
  function TermShell(container, fs) {
    this.fs = fs;
    this.history = [];
    this.historyIndex = -1;
    this.line = "";
    this.cursorPos = 0;
    this.savedLine = "";

    // Editing mode: "emacs" or "vi"
    this.editMode = "vi";
    // Vi sub-mode: "insert" or "command"
    this.viMode = "insert";
    // Pending vi operator: "d" or "c" or null
    this.viPending = null;
    // Kill ring for Ctrl+K/U/W/Y and vi yank
    this.killRing = "";

    this.term = new window.Terminal({
      theme: {
        background: "#ece5db",
        foreground: "#2d2a26",
        cursor: "#b5652b",
        cursorAccent: "#ece5db",
        selectionBackground: "#b5652b30",
        selectionForeground: "#2d2a26",
        black: "#2d2a26",
        red: "#c44030",
        green: "#6e7d3a",
        yellow: "#b5652b",
        blue: "#8a8078",
        magenta: "#9e5019",
        cyan: "#8a8078",
        white: "#f6f1eb",
        brightBlack: "#8a8078",
        brightRed: "#c44030",
        brightGreen: "#6e7d3a",
        brightYellow: "#b5652b",
        brightBlue: "#8a8078",
        brightMagenta: "#9e5019",
        brightCyan: "#8a8078",
        brightWhite: "#f6f1eb",
      },
      fontFamily: "'IBM Plex Mono', monospace",
      fontSize: 14,
      rows: 20,
      cursorStyle: "bar",
      cursorBlink: true,
      scrollback: 500,
      convertEol: true,
      allowProposedApi: true,
    });

    this.fitAddon = new window.FitAddon.FitAddon();
    this.term.loadAddon(this.fitAddon);
    this.term.open(container);
    this.fitAddon.fit();

    // Build mode toggle
    this.toggleEl = document.createElement("button");
    this.toggleEl.className = "playground-mode-toggle vi";
    this.toggleEl.textContent = "VI";
    this.toggleEl.title = "Switch editing mode";
    container.appendChild(this.toggleEl);

    var self = this;
    this.toggleEl.addEventListener("click", function () {
      if (self.editMode === "emacs") {
        self.setMode("vi");
      } else {
        self.setMode("emacs");
      }
    });

    window.addEventListener("resize", function () {
      self.fitAddon.fit();
      var screen = container.querySelector(".xterm-screen");
      if (screen) self.cellHeight = screen.clientHeight / self.term.rows;
      self.updatePinned();
    });

    this.term.onKey(function (ev) {
      self.handleKey(ev.key, ev.domEvent);
    });

    // Prevent xterm from eating paste
    container.addEventListener("paste", function (e) {
      var text = (e.clipboardData || window.clipboardData).getData("text");
      if (text) {
        // In vi command mode, switch to insert before pasting
        if (self.editMode === "vi" && self.viMode === "command") {
          self.viEnterInsert();
        }
        for (var i = 0; i < text.length; i++) {
          var ch = text[i];
          if (ch === "\r" || ch === "\n") {
            self.submit();
          } else if (ch >= " ") {
            self.insertChar(ch);
          }
        }
      }
    });

    // Pinned command header (sticky fold)
    this.pinnedEl = document.createElement("div");
    this.pinnedEl.className = "playground-pinned-cmd";
    this.pinnedEl.hidden = true;
    this.pinnedPromptEl = document.createElement("span");
    this.pinnedPromptEl.className = "pinned-prompt";
    this.pinnedPromptEl.textContent = "path";
    this.pinnedDollarEl = document.createElement("span");
    this.pinnedDollarEl.className = "pinned-dollar";
    this.pinnedDollarEl.textContent = " $ ";
    this.pinnedTextEl = document.createElement("span");
    this.pinnedTextEl.className = "pinned-text";
    this.pinnedEl.appendChild(this.pinnedPromptEl);
    this.pinnedEl.appendChild(this.pinnedDollarEl);
    this.pinnedEl.appendChild(this.pinnedTextEl);
    container.appendChild(this.pinnedEl);

    this.cmdPositions = [];

    // Scroll listener for pinned header
    setTimeout(function () {
      self.viewport = container.querySelector(".xterm-viewport");
      var screen = container.querySelector(".xterm-screen");
      if (screen) self.cellHeight = screen.clientHeight / self.term.rows;
      if (self.viewport) {
        self.viewport.addEventListener("scroll", function () {
          self.updatePinned();
        });
      }
    }, 0);
    this.term.onScroll(function () {
      setTimeout(function () {
        self.updatePinned();
      }, 0);
    });
  }

  TermShell.prototype.setMode = function (mode) {
    this.editMode = mode;
    this.toggleEl.textContent = mode.toUpperCase();
    if (mode === "vi") {
      this.viMode = "insert";
      this.viPending = null;
      this.term.options.cursorStyle = "bar";
      this.toggleEl.classList.add("vi");
    } else {
      this.viMode = "insert";
      this.viPending = null;
      this.term.options.cursorStyle = "block";
      this.toggleEl.classList.remove("vi");
    }
  };

  TermShell.prototype.viEnterInsert = function () {
    this.viMode = "insert";
    this.viPending = null;
    this.term.options.cursorStyle = "bar";
  };

  TermShell.prototype.viEnterCommand = function () {
    this.viMode = "command";
    this.viPending = null;
    this.term.options.cursorStyle = "block";
    // Clamp cursor to last char (vi command mode convention)
    if (this.cursorPos > 0 && this.cursorPos >= this.line.length) {
      this.cursorPos = Math.max(0, this.line.length - 1);
      this.refreshLine();
    }
  };

  // Delete a range and store in kill ring
  TermShell.prototype.killRange = function (from, to) {
    if (from === to) return;
    var start = Math.min(from, to);
    var end = Math.max(from, to);
    this.killRing = this.line.substring(start, end);
    this.line = this.line.substring(0, start) + this.line.substring(end);
    this.cursorPos = start;
    this.refreshLine();
  };

  TermShell.prototype.prompt = function () {
    this.term.write(copperBold("path") + " " + pencil("$") + " ");
    this.line = "";
    this.cursorPos = 0;
    // Reset vi to insert mode on new prompt
    if (this.editMode === "vi") {
      this.viEnterInsert();
    }
  };

  TermShell.prototype.refreshLine = function () {
    this.term.write("\r");
    this.term.write(copperBold("path") + " " + pencil("$") + " ");
    this.term.write(this.line);
    this.term.write("\x1b[K"); // clear to end of line
    var moveBack = this.line.length - this.cursorPos;
    if (moveBack > 0) {
      this.term.write("\x1b[" + moveBack + "D");
    }
  };

  TermShell.prototype.insertChar = function (ch) {
    this.line =
      this.line.substring(0, this.cursorPos) +
      ch +
      this.line.substring(this.cursorPos);
    this.cursorPos++;
    this.refreshLine();
  };

  TermShell.prototype.pinCommand = function (cmd) {
    this.cmdPositions.push({
      cmd: cmd,
      absY: this.term.buffer.active.baseY + this.term.buffer.active.cursorY,
    });
    this.updatePinned();
  };

  TermShell.prototype.updatePinned = function () {
    if (!this.cmdPositions.length || !this.viewport || !this.cellHeight) {
      this.pinnedEl.hidden = true;
      return;
    }
    var topLine = Math.floor(this.viewport.scrollTop / this.cellHeight);
    // Find the last command whose prompt scrolled above the viewport
    var pinned = null;
    for (var i = this.cmdPositions.length - 1; i >= 0; i--) {
      if (this.cmdPositions[i].absY < topLine) {
        pinned = this.cmdPositions[i];
        break;
      }
    }
    if (pinned) {
      this.pinnedTextEl.textContent = pinned.cmd;
      this.pinnedEl.hidden = false;
    } else {
      this.pinnedEl.hidden = true;
    }
  };

  TermShell.prototype.submit = function () {
    if (this.line.trim()) this.pinCommand(this.line);
    this.term.write("\r\n");
    var line = this.line;
    if (line.trim()) {
      this.history.push(line);
      if (this.history.length > 50) this.history.shift();
    }
    this.historyIndex = -1;
    this.savedLine = "";

    var self = this;
    var result = dispatch(line, this.fs);
    if (result && typeof result.then === "function") {
      // Async (wasm command) — disable input until done
      this.busy = true;
      result.then(function (r) {
        if (r && r.output != null) self.term.write(r.output + "\r\n");
        self.busy = false;
        self.prompt();
      });
    } else {
      if (result) {
        if (result.clear) {
          this.term.clear();
          this.cmdPositions = [];
          this.pinnedEl.hidden = true;
        } else if (result.output != null) {
          this.term.write(result.output + "\r\n");
        }
      }
      this.prompt();
    }
  };

  // --- History navigation (shared) ---
  TermShell.prototype.historyPrev = function () {
    if (this.history.length === 0) return;
    if (this.historyIndex === -1) {
      this.savedLine = this.line;
      this.historyIndex = this.history.length - 1;
    } else if (this.historyIndex > 0) {
      this.historyIndex--;
    }
    this.line = this.history[this.historyIndex];
    this.cursorPos = this.line.length;
    this.refreshLine();
  };

  TermShell.prototype.historyNext = function () {
    if (this.historyIndex === -1) return;
    if (this.historyIndex < this.history.length - 1) {
      this.historyIndex++;
      this.line = this.history[this.historyIndex];
    } else {
      this.historyIndex = -1;
      this.line = this.savedLine;
    }
    this.cursorPos = this.line.length;
    this.refreshLine();
  };

  // --- Key dispatch ---
  TermShell.prototype.handleKey = function (key, domEvent) {
    if (this.busy) return;
    var code = domEvent.keyCode;

    // Tab - always ignore
    if (code === 9) {
      domEvent.preventDefault();
      return;
    }

    // Enter - always submit
    if (code === 13) {
      // In vi command mode, move to insert before submitting for clean state
      this.submit();
      return;
    }

    // Ctrl+C - always cancel
    if (domEvent.ctrlKey && code === 67) {
      this.term.write("^C\r\n");
      this.prompt();
      return;
    }

    // Ctrl+L - always clear
    if (domEvent.ctrlKey && code === 76) {
      this.term.clear();
      this.cmdPositions = [];
      this.pinnedEl.hidden = true;
      this.refreshLine();
      return;
    }

    if (this.editMode === "emacs") {
      this.handleEmacs(key, domEvent, code);
    } else {
      this.handleVi(key, domEvent, code);
    }
  };

  // --- Emacs mode ---
  TermShell.prototype.handleEmacs = function (key, domEvent, code) {
    // --- Ctrl bindings ---
    if (domEvent.ctrlKey) {
      switch (code) {
        case 65: // Ctrl+A: beginning of line
          this.cursorPos = 0;
          this.refreshLine();
          return;
        case 69: // Ctrl+E: end of line
          this.cursorPos = this.line.length;
          this.refreshLine();
          return;
        case 66: // Ctrl+B: back one char
          if (this.cursorPos > 0) {
            this.cursorPos--;
            this.term.write("\x1b[D");
          }
          return;
        case 70: // Ctrl+F: forward one char
          if (this.cursorPos < this.line.length) {
            this.cursorPos++;
            this.term.write("\x1b[C");
          }
          return;
        case 68: // Ctrl+D: delete char at cursor (or no-op if empty)
          if (this.cursorPos < this.line.length) {
            this.line =
              this.line.substring(0, this.cursorPos) +
              this.line.substring(this.cursorPos + 1);
            this.refreshLine();
          }
          return;
        case 72: // Ctrl+H: backspace
          if (this.cursorPos > 0) {
            this.line =
              this.line.substring(0, this.cursorPos - 1) +
              this.line.substring(this.cursorPos);
            this.cursorPos--;
            this.refreshLine();
          }
          return;
        case 75: // Ctrl+K: kill to end of line
          this.killRing = this.line.substring(this.cursorPos);
          this.line = this.line.substring(0, this.cursorPos);
          this.refreshLine();
          return;
        case 85: // Ctrl+U: kill to beginning of line
          this.killRing = this.line.substring(0, this.cursorPos);
          this.line = this.line.substring(this.cursorPos);
          this.cursorPos = 0;
          this.refreshLine();
          return;
        case 87: // Ctrl+W: kill previous word
          var wb = wordBoundaryLeft(this.line, this.cursorPos);
          this.killRange(wb, this.cursorPos);
          return;
        case 89: // Ctrl+Y: yank
          if (this.killRing) {
            this.line =
              this.line.substring(0, this.cursorPos) +
              this.killRing +
              this.line.substring(this.cursorPos);
            this.cursorPos += this.killRing.length;
            this.refreshLine();
          }
          return;
        case 84: // Ctrl+T: transpose chars
          if (this.cursorPos > 0 && this.line.length > 1) {
            var p = this.cursorPos;
            if (p >= this.line.length) p = this.line.length - 1;
            var a = this.line[p - 1];
            var b = this.line[p];
            this.line =
              this.line.substring(0, p - 1) +
              b +
              a +
              this.line.substring(p + 1);
            this.cursorPos = p + 1;
            if (this.cursorPos > this.line.length)
              this.cursorPos = this.line.length;
            this.refreshLine();
          }
          return;
        case 80: // Ctrl+P: previous history
          this.historyPrev();
          return;
        case 78: // Ctrl+N: next history
          this.historyNext();
          return;
      }
      return;
    }

    // --- Alt/Meta bindings ---
    if (domEvent.altKey || domEvent.metaKey) {
      switch (code) {
        case 66: // Alt+B: back one word
          this.cursorPos = wordBoundaryLeft(this.line, this.cursorPos);
          this.refreshLine();
          return;
        case 70: // Alt+F: forward one word
          this.cursorPos = wordEndRight(this.line, this.cursorPos);
          this.refreshLine();
          return;
        case 68: // Alt+D: kill forward word
          var we = wordEndRight(this.line, this.cursorPos);
          this.killRange(this.cursorPos, we);
          return;
      }
      return;
    }

    // --- Regular keys ---
    switch (code) {
      case 8: // Backspace
        if (this.cursorPos > 0) {
          this.line =
            this.line.substring(0, this.cursorPos - 1) +
            this.line.substring(this.cursorPos);
          this.cursorPos--;
          this.refreshLine();
        }
        return;
      case 46: // Delete
        if (this.cursorPos < this.line.length) {
          this.line =
            this.line.substring(0, this.cursorPos) +
            this.line.substring(this.cursorPos + 1);
          this.refreshLine();
        }
        return;
      case 37: // Left
        if (this.cursorPos > 0) {
          this.cursorPos--;
          this.term.write("\x1b[D");
        }
        return;
      case 39: // Right
        if (this.cursorPos < this.line.length) {
          this.cursorPos++;
          this.term.write("\x1b[C");
        }
        return;
      case 38: // Up
        this.historyPrev();
        return;
      case 40: // Down
        this.historyNext();
        return;
      case 36: // Home
        this.cursorPos = 0;
        this.refreshLine();
        return;
      case 35: // End
        this.cursorPos = this.line.length;
        this.refreshLine();
        return;
    }

    // Printable
    if (key.length === 1 && key >= " ") {
      this.insertChar(key);
    }
  };

  // --- Vi mode ---
  TermShell.prototype.handleVi = function (key, domEvent, code) {
    if (this.viMode === "insert") {
      this.handleViInsert(key, domEvent, code);
    } else {
      this.handleViCommand(key, domEvent, code);
    }
  };

  // Vi insert mode — mostly like basic editing, Escape enters command mode
  TermShell.prototype.handleViInsert = function (key, domEvent, code) {
    // Escape → command mode
    if (code === 27) {
      // Step cursor back one (vi convention: cursor sits on last typed char)
      if (this.cursorPos > 0) this.cursorPos--;
      this.viEnterCommand();
      this.refreshLine();
      return;
    }

    // Ctrl bindings still useful in vi insert mode
    if (domEvent.ctrlKey) {
      switch (code) {
        case 85: // Ctrl+U: kill to beginning
          this.killRing = this.line.substring(0, this.cursorPos);
          this.line = this.line.substring(this.cursorPos);
          this.cursorPos = 0;
          this.refreshLine();
          return;
        case 87: // Ctrl+W: kill previous word
          var wb = wordBoundaryLeft(this.line, this.cursorPos);
          this.killRange(wb, this.cursorPos);
          return;
        case 72: // Ctrl+H: backspace
          if (this.cursorPos > 0) {
            this.line =
              this.line.substring(0, this.cursorPos - 1) +
              this.line.substring(this.cursorPos);
            this.cursorPos--;
            this.refreshLine();
          }
          return;
      }
      return;
    }

    if (domEvent.altKey || domEvent.metaKey) return;

    // Standard keys
    switch (code) {
      case 8: // Backspace
        if (this.cursorPos > 0) {
          this.line =
            this.line.substring(0, this.cursorPos - 1) +
            this.line.substring(this.cursorPos);
          this.cursorPos--;
          this.refreshLine();
        }
        return;
      case 46: // Delete
        if (this.cursorPos < this.line.length) {
          this.line =
            this.line.substring(0, this.cursorPos) +
            this.line.substring(this.cursorPos + 1);
          this.refreshLine();
        }
        return;
      case 37:
        if (this.cursorPos > 0) {
          this.cursorPos--;
          this.term.write("\x1b[D");
        }
        return;
      case 39:
        if (this.cursorPos < this.line.length) {
          this.cursorPos++;
          this.term.write("\x1b[C");
        }
        return;
      case 38:
        this.historyPrev();
        return;
      case 40:
        this.historyNext();
        return;
      case 36:
        this.cursorPos = 0;
        this.refreshLine();
        return;
      case 35:
        this.cursorPos = this.line.length;
        this.refreshLine();
        return;
    }

    // Printable
    if (key.length === 1 && key >= " ") {
      this.insertChar(key);
    }
  };

  // Vi command mode — single-char and operator+motion commands
  TermShell.prototype.handleViCommand = function (key, domEvent, code) {
    // Escape always cancels pending operator
    if (code === 27) {
      this.viPending = null;
      return;
    }

    if (domEvent.ctrlKey || domEvent.altKey || domEvent.metaKey) return;

    // Arrow keys still work in command mode
    switch (code) {
      case 37:
        if (this.cursorPos > 0) {
          this.cursorPos--;
          this.refreshLine();
        }
        this.viPending = null;
        return;
      case 39:
        if (this.cursorPos < this.line.length - 1) {
          this.cursorPos++;
          this.refreshLine();
        }
        this.viPending = null;
        return;
      case 38:
        this.historyPrev();
        this.viPending = null;
        return;
      case 40:
        this.historyNext();
        this.viPending = null;
        return;
      case 8: // Backspace
        if (this.cursorPos > 0) {
          this.cursorPos--;
          this.refreshLine();
        }
        this.viPending = null;
        return;
    }

    // Only handle single printable chars from here
    if (key.length !== 1) return;

    var pending = this.viPending;

    // Handle pending operator + motion
    if (pending === "d" || pending === "c") {
      this.viPending = null;
      var enterInsert = pending === "c";
      switch (key) {
        case "w": // delete/change word forward
          var we = wordEndRight(this.line, this.cursorPos);
          this.killRange(this.cursorPos, we);
          if (enterInsert) this.viEnterInsert();
          return;
        case "b": // delete/change word backward
          var wb = wordBoundaryLeft(this.line, this.cursorPos);
          this.killRange(wb, this.cursorPos);
          if (enterInsert) this.viEnterInsert();
          return;
        case "e": // delete/change to end of word
          var wee = wordEndRight(this.line, this.cursorPos);
          this.killRange(this.cursorPos, wee);
          if (enterInsert) this.viEnterInsert();
          return;
        case "W": // delete/change WORD forward
          var Wt = WORDBoundaryRight(this.line, this.cursorPos);
          this.killRange(this.cursorPos, Wt);
          if (enterInsert) this.viEnterInsert();
          return;
        case "B": // delete/change WORD backward
          var Bt = WORDBoundaryLeft(this.line, this.cursorPos);
          this.killRange(Bt, this.cursorPos);
          if (enterInsert) this.viEnterInsert();
          return;
        case "E": // delete/change to end of WORD
          var Et = WORDEndRight(this.line, this.cursorPos);
          this.killRange(this.cursorPos, Et);
          if (enterInsert) this.viEnterInsert();
          return;
        case "$": // delete/change to end of line
          this.killRange(this.cursorPos, this.line.length);
          if (enterInsert) this.viEnterInsert();
          return;
        case "0": // delete/change to beginning of line
          this.killRange(0, this.cursorPos);
          if (enterInsert) this.viEnterInsert();
          return;
        case "d": // dd: delete whole line
          if (pending === "d") {
            this.killRing = this.line;
            this.line = "";
            this.cursorPos = 0;
            this.refreshLine();
          }
          return;
        case "c": // cc: change whole line
          if (pending === "c") {
            this.killRing = this.line;
            this.line = "";
            this.cursorPos = 0;
            this.refreshLine();
            this.viEnterInsert();
          }
          return;
      }
      // Unknown motion — cancel
      return;
    }

    // Single-char commands
    switch (key) {
      // --- Movement ---
      case "h":
        if (this.cursorPos > 0) {
          this.cursorPos--;
          this.refreshLine();
        }
        return;
      case "l":
        if (this.cursorPos < this.line.length - 1) {
          this.cursorPos++;
          this.refreshLine();
        }
        return;
      case "w":
        this.cursorPos = Math.min(
          wordBoundaryRight(this.line, this.cursorPos),
          Math.max(0, this.line.length - 1),
        );
        this.refreshLine();
        return;
      case "b":
        this.cursorPos = wordBoundaryLeft(this.line, this.cursorPos);
        this.refreshLine();
        return;
      case "e":
        var ep = wordEndRight(this.line, this.cursorPos);
        this.cursorPos = Math.min(
          Math.max(0, ep - 1),
          Math.max(0, this.line.length - 1),
        );
        this.refreshLine();
        return;
      case "W":
        this.cursorPos = Math.min(
          WORDBoundaryRight(this.line, this.cursorPos),
          Math.max(0, this.line.length - 1),
        );
        this.refreshLine();
        return;
      case "B":
        this.cursorPos = WORDBoundaryLeft(this.line, this.cursorPos);
        this.refreshLine();
        return;
      case "E":
        var Ep = WORDEndRight(this.line, this.cursorPos);
        this.cursorPos = Math.min(
          Math.max(0, Ep - 1),
          Math.max(0, this.line.length - 1),
        );
        this.refreshLine();
        return;
      case "0":
        this.cursorPos = 0;
        this.refreshLine();
        return;
      case "^":
        // First non-whitespace
        var fi = 0;
        while (
          fi < this.line.length &&
          (this.line[fi] === " " || this.line[fi] === "\t")
        )
          fi++;
        this.cursorPos = Math.min(fi, Math.max(0, this.line.length - 1));
        this.refreshLine();
        return;
      case "$":
        this.cursorPos = Math.max(0, this.line.length - 1);
        this.refreshLine();
        return;

      // --- Insert mode entry ---
      case "i":
        this.viEnterInsert();
        return;
      case "a":
        if (this.line.length > 0)
          this.cursorPos = Math.min(this.cursorPos + 1, this.line.length);
        this.viEnterInsert();
        this.refreshLine();
        return;
      case "I":
        this.cursorPos = 0;
        this.viEnterInsert();
        this.refreshLine();
        return;
      case "A":
        this.cursorPos = this.line.length;
        this.viEnterInsert();
        this.refreshLine();
        return;
      case "s": // substitute char: delete + insert
        if (this.cursorPos < this.line.length) {
          this.killRing = this.line[this.cursorPos];
          this.line =
            this.line.substring(0, this.cursorPos) +
            this.line.substring(this.cursorPos + 1);
          this.refreshLine();
        }
        this.viEnterInsert();
        return;
      case "S": // substitute line
        this.killRing = this.line;
        this.line = "";
        this.cursorPos = 0;
        this.refreshLine();
        this.viEnterInsert();
        return;

      // --- Deletion ---
      case "x": // delete char at cursor
        if (this.cursorPos < this.line.length) {
          this.killRing = this.line[this.cursorPos];
          this.line =
            this.line.substring(0, this.cursorPos) +
            this.line.substring(this.cursorPos + 1);
          if (this.cursorPos >= this.line.length && this.cursorPos > 0)
            this.cursorPos--;
          this.refreshLine();
        }
        return;
      case "X": // delete char before cursor
        if (this.cursorPos > 0) {
          this.killRing = this.line[this.cursorPos - 1];
          this.line =
            this.line.substring(0, this.cursorPos - 1) +
            this.line.substring(this.cursorPos);
          this.cursorPos--;
          this.refreshLine();
        }
        return;
      case "D": // delete to end of line
        this.killRange(this.cursorPos, this.line.length);
        if (this.cursorPos > 0 && this.cursorPos >= this.line.length)
          this.cursorPos--;
        this.refreshLine();
        return;
      case "C": // change to end of line
        this.killRange(this.cursorPos, this.line.length);
        this.viEnterInsert();
        return;

      // --- Operators (wait for motion) ---
      case "d":
        this.viPending = "d";
        return;
      case "c":
        this.viPending = "c";
        return;

      // --- Yank/paste ---
      case "p": // paste after cursor
        if (this.killRing) {
          var pos = Math.min(this.cursorPos + 1, this.line.length);
          this.line =
            this.line.substring(0, pos) +
            this.killRing +
            this.line.substring(pos);
          this.cursorPos = pos + this.killRing.length - 1;
          this.refreshLine();
        }
        return;
      case "P": // paste before cursor
        if (this.killRing) {
          this.line =
            this.line.substring(0, this.cursorPos) +
            this.killRing +
            this.line.substring(this.cursorPos);
          this.cursorPos += this.killRing.length - 1;
          this.refreshLine();
        }
        return;

      // --- History ---
      case "k":
        this.historyPrev();
        return;
      case "j":
        this.historyNext();
        return;

      // --- Replace single char ---
      // 'r' would need a follow-up char — skip for simplicity
    }
  };

  // Execute a command programmatically and write output
  TermShell.prototype.exec = function (line, callback) {
    this.term.write(line);
    if (line.trim()) this.pinCommand(line);
    this.term.write("\r\n");
    this.line = line;
    if (line.trim()) {
      this.history.push(line);
    }
    var self = this;
    var result = dispatch(line, this.fs);
    if (result && typeof result.then === "function") {
      result.then(function (r) {
        if (r && r.output != null) self.term.write(r.output + "\r\n");
        if (callback) callback();
      });
    } else {
      if (result) {
        if (result.clear) {
          this.term.clear();
        } else if (result.output != null) {
          this.term.write(result.output + "\r\n");
        }
      }
      if (callback) callback();
    }
  };

  // Auto-type a command character by character, then execute
  TermShell.prototype.autoType = function (line, callback) {
    var self = this;
    var i = 0;
    function typeNext() {
      if (i < line.length) {
        self.term.write(line[i]);
        i++;
        setTimeout(typeNext, 30);
      } else {
        if (line.trim()) self.pinCommand(line);
        self.term.write("\r\n");
        self.line = line;
        if (line.trim()) {
          self.history.push(line);
        }
        var result = dispatch(line, self.fs);
        if (result && typeof result.then === "function") {
          result.then(function (r) {
            if (r && r.output != null) self.term.write(r.output + "\r\n");
            if (callback) callback();
          });
        } else {
          if (result && result.output != null) {
            self.term.write(result.output + "\r\n");
          }
          if (callback) callback();
        }
      }
    }
    typeNext();
  };

  // --- Boot sequence ---
  function boot() {
    var el = document.getElementById("playground-terminal");
    if (!el) return;

    var files = window.__PLAYGROUND_FILES__ || {};
    var fs = new VirtualFS(files);
    var shell = new TermShell(el, fs);

    // Banner
    shell.term.write(
      copperBold("TOOLPATH PLAYGROUND") +
        "  " +
        dim("interactive terminal") +
        "\r\n",
    );
    shell.term.write(
      dim("Type help for commands. Example documents are preloaded.") + "\r\n",
    );
    shell.term.write("\r\n");

    // Suggested commands (dimmed)
    var suggestions = [
      "path validate --input path-01-pr.json",
      "path query dead-ends --input path-01-pr.json --pretty",
      'path query filter --input path-01-pr.json --actor "agent:" --pretty',
    ];
    for (var i = 0; i < suggestions.length; i++) {
      shell.term.write(dim("  # " + suggestions[i]) + "\r\n");
    }
    shell.term.write("\r\n");

    // Load wasm in background, then auto-type a command
    shell.term.write(dim("  Loading CLI...") + "\r\n\r\n");
    loadWasm(files)
      .then(function () {
        shell.term.write(copperBold("path") + " " + pencil("$") + " ");
        shell.autoType(
          "path query dead-ends --input path-01-pr.json --pretty",
          function () {
            shell.prompt();
          },
        );
      })
      .catch(function () {
        shell.term.write(red("  Failed to load wasm binary.") + "\r\n\r\n");
        shell.prompt();
      });
  }

  // Initialize when DOM is ready
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }
})();
