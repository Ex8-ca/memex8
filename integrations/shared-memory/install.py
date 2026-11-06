#!/usr/bin/env python3
"""Install the memex8 bridge for local Codex and Claude Code clients."""
import argparse
from datetime import datetime, timezone
import getpass
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', required=True)
    parser.add_argument('--base-url', default='http://127.0.0.1:8080')
    parser.add_argument('--allow-lan-http', action='store_true')
    parser.add_argument('--vault', type=Path)
    parser.add_argument('--clients', nargs='+', choices=['codex', 'claude'], default=['codex', 'claude'])
    parser.add_argument('--key-env-file', type=Path, help='Read only MEMEX8_API_KEY from an existing dotenv file; never execute it')
    parser.add_argument('--check-only', action='store_true', help='Check API access without installing or changing client settings')
    args = parser.parse_args()
    os.umask(0o077)
    key = os.environ.get('MEMEX8_API_KEY')
    if args.key_env_file:
        for line in args.key_env_file.expanduser().read_text().splitlines():
            name, sep, value = line.strip().partition('=')
            if sep and name.removeprefix('export ').strip() == 'MEMEX8_API_KEY':
                key = value.strip()
                if len(key) >= 2 and key[0] == key[-1] and key[0] in "\"'":
                    key = key[1:-1]
    if not key:
        key = getpass.getpass('memex8 API key: ')
    source = Path(__file__).resolve().with_name('bridge.py')
    spec = importlib.util.spec_from_file_location('memex8_bridge', source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    vault = args.vault.expanduser().resolve() if args.vault else None
    bridge = module.Bridge(args.base_url, key, 'codex', args.project, vault=vault,
                           allow_lan_http=args.allow_lan_http)
    print(bridge.check())
    if args.check_only:
        return
    for client in args.clients:
        if not shutil.which(client):
            raise ValueError(f'{client} is not installed or is absent from PATH; use --clients to select installed clients')
    directory = Path.home() / '.local/share/memex8'
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    keyfile = directory / 'api-key'
    if keyfile.is_symlink():
        raise ValueError('Refusing a symlink API key file')
    # Open with restrictive permissions even when replacing an existing key.
    fd = os.open(keyfile, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, 'w') as stream:
        os.chmod(keyfile, 0o600)
        stream.write(key + '\n')
    target = directory / 'bridge.py'
    if target.is_symlink():
        raise ValueError('Refusing a symlink bridge file')
    shutil.copyfile(source, target)
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    for client in args.clients:
        config = Path.home() / ('.codex/config.toml' if client == 'codex' else '.claude.json')
        if config.exists():
            saved = directory / (client + '-config-' + stamp + '.bak')
            shutil.copyfile(config, saved)
            os.chmod(saved, 0o600)
            print('Saved original client settings:', saved)
        command = [sys.executable, str(target), '--agent', 'codex' if client == 'codex' else 'claude-code',
                   '--project', args.project, '--base-url', args.base_url, '--api-key-file', str(keyfile)]
        if args.allow_lan_http:
            command.append('--allow-lan-http')
        if vault:
            command += ['--vault', str(vault)]
        register = [client, 'mcp', 'add']
        if client == 'claude':
            register += ['--transport', 'stdio', '--scope', 'user']
        subprocess.run(register + ['memex8', '--'] + command, check=True)
        print('Registered memex8 for', client)
    print('Restart the selected clients to load memex8. Settings backups and the private API key are in', directory)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as exc:
        print('Setup failed:', str(exc), file=sys.stderr)
        sys.exit(1)
