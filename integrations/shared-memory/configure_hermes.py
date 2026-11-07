#!/usr/bin/env python3
"""Point an existing Hermes memex8 plugin at the shared memory server."""
import argparse
from datetime import datetime, timezone
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import sys
import tempfile


def update_env(text, values):
    lines = []
    for line in text.splitlines():
        name = line.strip().removeprefix('export ').partition('=')[0].strip()
        if name not in values:
            lines.append(line)
    return '\n'.join(lines + [name + '=' + shlex.quote(value) for name, value in values.items()]) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-url', required=True)
    parser.add_argument('--allow-lan-http', action='store_true')
    parser.add_argument('--api-key-file', type=Path, default=Path.home()/'.local/share/memex8/api-key')
    parser.add_argument('--hermes-home', type=Path, default=Path(os.environ.get('HERMES_HOME', Path.home()/'.hermes')))
    parser.add_argument('--check-only', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    home = args.hermes_home.expanduser().resolve()
    if not home.is_dir():
        raise ValueError('Hermes home directory does not exist')
    key = args.api_key_file.expanduser().read_text().strip()
    spec = importlib.util.spec_from_file_location('bridge', Path(__file__).with_name('bridge.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    bridge = module.Bridge(args.base_url, key, 'hermes', 'connection-check', allow_lan_http=args.allow_lan_http)
    print(bridge.check())
    if args.check_only:
        return
    settings = home/'memex8.json'
    config = json.loads(settings.read_text()) if settings.exists() else {}
    config.update(base_url=args.base_url.rstrip('/'), api_key=key)
    values = {'MEMEX8_BASE_URL': config['base_url'], 'MEMEX8_API_KEY': key}
    changes = {settings: json.dumps(config, indent=2)+'\n'}
    env = home/'.env'
    changes[env] = update_env(env.read_text() if env.exists() else '', values)
    keyfile = home/'memex8_api.key'
    if keyfile.exists():
        changes[keyfile] = key+'\n'
    shell = home/'memex8_env.sh'
    if shell.exists():
        changes[shell] = update_env(shell.read_text(), {**values, 'MEMEX8_API_BASE': config['base_url']})
        changes[shell] += 'export MEMEX8_BASE_URL MEMEX8_API_KEY MEMEX8_API_BASE\n'
    for path in changes:
        if path.is_symlink():
            raise ValueError('Refusing to replace a symlink: '+str(path))
    backup = home/('memex8-settings-backup-'+datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ'))
    backup.mkdir(mode=0o700)
    for path in changes:
        if path.exists():
            shutil.copyfile(path, backup/path.name)
    for path, text in changes.items():
        fd, temporary = tempfile.mkstemp(dir=home, prefix='.memex8-update-')
        try:
            with os.fdopen(fd, 'w') as stream:
                stream.write(text)
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    print('Hermes memex8 configured for', config['base_url'])
    print('Original settings saved in', backup)
    print('Restart Hermes from a fresh terminal. Existing shells may retain old MEMEX8 environment variables.')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError) as exc:
        print('Hermes setup failed:', str(exc), file=sys.stderr)
        sys.exit(1)
