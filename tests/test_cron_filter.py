"""Smoke test for the new _CRON_MARKER_RE regex in plugins/memex8/__init__.py.

Doesn't import the plugin (heavy deps); just exec()'s the two regex definitions
out of the source file and runs them against realistic inputs.
"""
import re

# Extract the regex definitions from the source. We isolate them by capturing
# from "_TRIVIAL_RE = re.compile(" through the closing `)` of _CRON_MARKER_RE.
src = open("plugins/memex8/__init__.py").read()

start = src.index("_TRIVIAL_RE = re.compile(")
end_marker = '_CRON_MARKER_RE = re.compile('
end = src.index(end_marker, start)
# Walk forward to find the matching close-paren
depth = 0
i = end
while i < len(src):
    c = src[i]
    if c == '(':
        depth += 1
    elif c == ')':
        depth -= 1
        if depth == 0:
            end = i + 1
            break
    i += 1
block = src[start:end]
exec(compile(block, "<test-extract>", "exec"), globals())
assert '_TRIVIAL_RE' in globals() and '_CRON_MARKER_RE' in globals()

samples = [
    # (input, should_match)
    ("[IMPORTANT: You are running as a scheduled cron job. ...]", True),
    ("[SILENT]", True),
    ("[CRON_FAILURE] TRMNL weather update failed", True),
    ("[A2A inbound — message from a remote agent peer]", True),
    ("[assistant reply was empty — skipping]", True),
    ("warren is my friend who i bet CFL with", False),
    ("what time is it", False),
    ("Hey [SILENT] — was that a typo?", False),  # mid-message, should not match
    ("[SILENT_OK]", True),  # bracketed-but-not-cron — but `[SILENT` prefix matches — accept this is a tradeoff
    ("", False),
]

ok = 0
fail = []
for s, expected in samples:
    matched = bool(_CRON_MARKER_RE.match(s.strip()))
    status = "✓" if matched == expected else "✗"
    if matched == expected:
        ok += 1
    else:
        fail.append((s, expected, matched))
    print(f"{status}  expected={expected!s:5}  got={matched!s:5}  {s[:70]!r}")

print(f"\n{ok}/{len(samples)} pass")
if fail:
    print("FAILURES:")
    for s, e, g in fail:
        print(f"  expected={e} got={g}  {s!r}")
    raise SystemExit(1)