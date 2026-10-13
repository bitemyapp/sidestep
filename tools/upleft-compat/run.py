#!/usr/bin/env python3
"""Build an archived Upleft checkout against this Sidestep's objc2 ABI."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


def command(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--upleft', type=Path, default=ROOT.parent / 'upleft')
    parser.add_argument('--vendor-root', type=Path, help='Initialized Upleft vendor directory at the same submodule pins')
    parser.add_argument('--prepare-only', action='store_true')
    parser.add_argument('--smoke', type=Path, help='Build the GUI executable and open this document without showing a window')
    args, cargo = parser.parse_known_args()
    if cargo[:1] == ['--']:
        cargo = cargo[1:]
    if args.smoke and cargo:
        parser.error('--smoke cannot be combined with a Cargo command')
    cargo = ['build', '-p', 'upleft', '--bin', 'upleft'] if args.smoke else cargo or ['test', '-p', 'upleft-app', '--test', 'file_watcher_tests']
    smoke = args.smoke.resolve() if args.smoke else None
    if smoke and not smoke.is_file():
        parser.error(f'{smoke} must be an existing document')
    source = args.upleft.resolve()
    vendor = (args.vendor_root or source / 'vendor').resolve()
    revision = command('git', '-C', str(source), 'rev-parse', 'HEAD')
    # Do not silently substitute a different version of a reference library.
    for line in command('git', '-C', str(source), 'ls-tree', 'HEAD:vendor').splitlines():
        header, name = line.split('\t')
        mode, kind, sha = header.split()
        if kind != 'commit':
            continue
        checkout = vendor / name
        if not checkout.is_dir() or command('git', '-C', str(checkout), 'rev-parse', 'HEAD') != sha:
            parser.error(f'{checkout} must be initialized at {sha}; supply --vendor-root if needed')
    patches = sorted((HERE / 'patches').glob('*.patch'))
    digest = hashlib.sha256(Path(__file__).read_bytes() + revision.encode() + str(vendor).encode())
    for patch in patches:
        digest.update(patch.read_bytes())
    stage = ROOT / 'target' / ('upleft-objc2-' + digest.hexdigest()[:12])
    marker = stage / '.prepared'
    if not marker.exists():
        if stage.exists():
            parser.error(f'{stage} is an incomplete preparation; inspect and move it aside before retrying')
        stage.mkdir(parents=True)
        archive = subprocess.check_output(['git', '-C', str(source), 'archive', revision])
        with tarfile.open(fileobj=io.BytesIO(archive)) as files:
            files.extractall(stage, filter='data')
        # Git archives contain empty submodule directories; point each at its
        # verified checkout without writing to any reference library.
        for child in (stage / 'vendor').iterdir():
            if child.is_dir():
                child.rmdir()
                child.symlink_to(vendor / child.name, target_is_directory=True)
        for patch in patches:
            subprocess.run(['git', 'apply', '--unsafe-paths', '--directory=' + str(stage), str(patch)], cwd='/tmp', check=True)
        for manifest in (stage / 'crates').glob('*/Cargo.toml'):
            manifest.write_text(manifest.read_text() + '\n[target.\'cfg(target_os = "linux")\'.dependencies.sidestep]\npath = ' + json.dumps(str(ROOT / 'crates/sidestep')) + '\n')
            entries = [manifest.parent / 'src/lib.rs', manifest.parent / 'src/main.rs']
            entries += list(manifest.parent.glob('src/bin/**/*.rs'))
            for entry in entries:
                if entry.is_file() and (entry.name in ('lib.rs', 'main.rs') or entry.parent == manifest.parent / 'src/bin'):
                    entry.write_text(entry.read_text() + '\n#[cfg(target_os = "linux")]\nuse sidestep as _;\n')
        marker.write_text(revision + '\n')
    subprocess.run(['cargo', 'run', '--release', '--manifest-path', str(ROOT / 'tools/objc2-overlay/Cargo.toml'), '--', '--quiet'], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    config = '[patch.crates-io]\n'
    for manifest in sorted((ROOT / '.objc2-overlay').glob('*/Cargo.toml')):
        name = tomllib.loads(manifest.read_text())['package']['name']
        config += name + ' = { path = ' + json.dumps(str(manifest.parent)) + ' }\n'
    config += '\n[profile.dev.package.tiny-skia]\noverflow-checks = false\n'
    (stage / 'sidestep-cargo.toml').write_text(config)
    print(f'Upleft {revision}\nPrepared workspace: {stage}', flush=True)
    if args.prepare_only:
        return
    env = os.environ.copy()
    env.pop('DISPLAY', None)
    env.pop('WAYLAND_DISPLAY', None)
    env.setdefault('SIDESTEP_BACKEND', 'null')
    env.setdefault('CARGO_BUILD_JOBS', '4')
    env.setdefault('CARGO_PROFILE_DEV_DEBUG', '0')
    env.setdefault('CARGO_INCREMENTAL', '0')
    env.setdefault('CARGO_TARGET_DIR', str(ROOT / 'target/upleft-objc2-build'))
    subprocess.run(['cargo', '--config', str(stage / 'sidestep-cargo.toml'), *cargo, '--manifest-path', str(stage / 'Cargo.toml')], cwd=stage, env=env, check=True)
    if smoke:
        state = stage / 'smoke-state'
        for variable, directory in [('XDG_CONFIG_HOME', 'config'), ('XDG_CACHE_HOME', 'cache'), ('XDG_DATA_HOME', 'data')]:
            path = state / directory
            path.mkdir(parents=True, exist_ok=True)
            env[variable] = str(path)
        env['UPLEFT_HEADLESS_SMOKE'] = str(smoke)
        target = Path(env['CARGO_TARGET_DIR'])
        if not target.is_absolute():
            target = stage / target
        subprocess.run([str(target / 'debug/upleft')], cwd=stage, env=env, check=True, timeout=45)


if __name__ == '__main__':
    main()
