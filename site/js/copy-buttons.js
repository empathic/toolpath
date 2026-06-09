// Copy-to-clipboard for install commands. Buttons carry the literal command
// in data-copy; clicking writes it to the clipboard and flips the icon to a
// checkmark for a moment. Progressive enhancement: without JS the commands are
// still readable and selectable.
(function () {
  "use strict";

  function flashCopied(btn) {
    btn.classList.add("copied");
    btn.setAttribute("aria-label", "Copied to clipboard");
    window.setTimeout(function () {
      btn.classList.remove("copied");
      btn.setAttribute("aria-label", "Copy command to clipboard");
    }, 1500);
  }

  function legacyCopy(text) {
    var ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "absolute";
    ta.style.left = "-9999px";
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } catch (err) {
      /* nothing else to try */
    }
    document.body.removeChild(ta);
  }

  function copy(text, btn) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(
        function () {
          flashCopied(btn);
        },
        function () {
          legacyCopy(text);
          flashCopied(btn);
        }
      );
    } else {
      legacyCopy(text);
      flashCopied(btn);
    }
  }

  document.addEventListener("click", function (e) {
    var btn = e.target.closest(".copy-btn");
    if (!btn) return;
    var text = btn.getAttribute("data-copy");
    if (text) copy(text, btn);
  });
})();
