"""Private-span redaction for text the plugin sends to memex8.

Tag matching is ASCII-only, like the server's parser (src/engine/redaction.rs);
tests/fixtures/redaction_cases.json holds the cases both implementations pass.
"""
from __future__ import annotations

import re


class _PrivateRedactor:
    """State spans ordered messages/fields, including content later filtered out."""

    def __init__(self):
        self.depth = 0

    def text(self, text: str) -> str:
        out = []
        pos = 0
        # re.ASCII keeps tag matching ASCII-only, like the server's Rust parser.
        # Without it, Unicode case folding makes `</prıvate>` (dotless i) match,
        # and the code below treats any non-opening match as a closing tag, so
        # a lookalike would end a private span early and expose what follows.
        for match in re.finditer(r"</?private>", text, re.IGNORECASE | re.ASCII):
            if not self.depth:
                out.append(text[pos:match.start()])
            if match.group().lower() == "<private>":
                if not self.depth:
                    out.append("[private]")
                self.depth += 1
            elif self.depth:
                self.depth -= 1
            else:
                out.append(match.group())
            pos = match.end()
        if not self.depth:
            out.append(text[pos:])
        return "".join(out)

    def value(self, value):
        if isinstance(value, str):
            return self.text(value)
        if isinstance(value, list):
            return [self.value(item) for item in value]
        if isinstance(value, dict):
            # Keys can contain private data too; preserve insertion order.
            clean = {}
            for key, item in value.items():
                # Structural keys are not part of the ordered text stream.
                clean_key = _PrivateRedactor().text(key)
                clean_item = self.value(item)
                if clean_key in clean:
                    raise ValueError("redacted metadata keys collide")
                clean[clean_key] = clean_item
            return clean
        return value
