#!/usr/bin/env python3
"""Install/upgrade from source or an explicitly supplied binary. No service restart."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import sqlite3
from evidence_common import connection
import subprocess
import sys
import tempfile
from urllib.parse import unquote

HOOKS={'SessionStart':'session-start.sh','PostToolUse':'post-tool-use.sh','Stop':'stop.sh',
       'PreCompact':'session-end.sh','SessionEnd':'session-close.sh'}


def atomic(path,data,mode=0o600):
    path=Path(path); path.parent.mkdir(parents=True,exist_ok=True)
    fd,name=tempfile.mkstemp(prefix='.'+path.name+'-',dir=path.parent)
    try:
        with os.fdopen(fd,'wb') as stream:
            stream.write(data); stream.flush(); os.fsync(stream.fileno())
        os.chmod(name,mode)
        os.replace(name,path)
    finally:
        if os.path.exists(name): os.unlink(name)


def merge_hooks(settings,hook_dir):
    hooks=settings.setdefault('hooks',{})
    if not isinstance(hooks,dict): raise ValueError('existing hooks must be an object')
    for event,script in HOOKS.items():
        command=shlex.quote(str(hook_dir/script))
        groups=hooks.setdefault(event,[])
        if not isinstance(groups,list): raise ValueError('hook event must be an array')
        # Upgrade only exact IronMem-owned commands; preserve unrelated commands,
        # matchers and mixed groups. Legacy flat commands are recognized too.
        owned={command,str(hook_dir/script)}
        cleaned=[]
        for group in groups:
            if group.get('type')=='command' and group.get('command') in owned: continue
            if isinstance(group.get('hooks'),list):
                kept=[h for h in group['hooks'] if not(h.get('type')=='command' and h.get('command') in owned)]
                if kept: cleaned.append(dict(group,hooks=kept))
            else: cleaned.append(group)
        cleaned.append({'hooks':[{'type':'command','command':command}]})
        hooks[event]=cleaned
    return settings


def path_instructions(binary_path):
    """Tell the operator how to reach the binary. The installer does not edit PATH."""
    binary_path=Path(binary_path)
    bindir=str(binary_path.parent)
    binary=str(binary_path)
    on_path=bindir in os.environ.get('PATH','').split(os.pathsep)
    state='That directory is already on PATH for this process.' if on_path else 'That directory is not on PATH for this process.'
    if os.name=='nt':
        return (
            f'Installed {binary}. This installer does not modify PATH. {state}\n'
            f'Current session: $env:Path = "{bindir};" + $env:Path\n'
            'User PATH for new terminals:\n'
            f'  [Environment]::SetEnvironmentVariable("Path", "{bindir};" + [Environment]::GetEnvironmentVariable("Path","User"), "User")\n'
            'Cursor and other GUI apps may not expand ~ and may not inherit a shell PATH. '
            f'In MCP settings set command to this absolute path: {binary}\n'
            'Optional: rerun with --add-to-path DIR to symlink the binary into DIR.'
        )
    return (
        f'Installed {binary}. This installer does not modify PATH. {state}\n'
        'Add this to ~/.zshrc or ~/.bashrc, then open a new terminal:\n'
        f'  export PATH="{bindir}:$PATH"\n'
        'Or rerun with --add-to-path to symlink into ~/.local/bin (only helps if that directory is already on PATH).\n'
        'Cursor launched from a desktop icon often does not inherit shell PATH, and it may not expand ~ in MCP config. '
        f'Use command "ironmem" only when PATH is visible to Cursor, or set command to this absolute path: {binary}\n'
        'The same absolute command is written to mcp.json beside the binary.'
    )


def link_binary(target, link_dir):
    link_dir=Path(link_dir).expanduser().resolve()
    link_dir.mkdir(parents=True, exist_ok=True)
    link=link_dir/('ironmem.exe' if os.name=='nt' else 'ironmem')
    target=Path(target).resolve()
    if link.is_symlink() and link.resolve()==target:
        return str(link)
    if link.exists() or link.is_symlink():
        raise OSError(f'{link} exists and is not an IronMem symlink; remove it or choose another directory')
    os.symlink(target, link)
    return str(link)


def install(source,prefix,claude_dir,binary=None,no_hooks=False,external_backup_confirmed=False,link_dir=None):
    source=Path(source).resolve(); prefix=Path(prefix).resolve(); claude_dir=Path(claude_dir).resolve()
    if not no_hooks and prefix != (Path.home()/'.ironmem').resolve():
        no_hooks=True  # Custom installs use the absolute --config MCP descriptor.
    if binary is None:
        subprocess.run(['cargo','build','--release','--locked'],cwd=source,check=True)
        binary=source/'target'/'release'/('ironmem.exe' if os.name=='nt' else 'ironmem')
    binary=Path(binary).resolve()
    subprocess.run([str(binary),'--version'],check=True,capture_output=True)
    config_path=prefix/'settings.json'
    if os.environ.get("DATABASE_URL") and not external_backup_confirmed:
        raise ValueError("DATABASE_URL override requires an operator-verified backup and --external-backup-confirmed")
    config=json.loads(config_path.read_text()) if config_path.exists() else {'db_path':str(prefix/'mem.db')}
    settings_path=claude_dir/'settings.json'
    settings=json.loads(settings_path.read_text()) if settings_path.exists() else {}
    # Complete validation before replacing any installed file.
    if not no_hooks and os.name!='nt': settings=merge_hooks(settings,claude_dir/'hooks')
    target=prefix/'bin'/('ironmem.exe' if os.name=='nt' else 'ironmem')
    writes={target:(binary.read_bytes(),0o755)}
    if not config_path.exists(): writes[config_path]=(json.dumps(config,indent=2).encode()+b'\n',0o600)
    descriptor={'mcpServers':{'ironmem':{'command':str(target),'args':['--config',str(config_path),'mcp']}}}
    writes[prefix/'mcp.json']=(json.dumps(descriptor,indent=2).encode()+b'\n',0o600)
    if not no_hooks and os.name!='nt':
        for name in ['lib.sh',*HOOKS.values()]: writes[claude_dir/'hooks'/name]=((source/'hooks'/name).read_bytes(),0o755)
        writes[settings_path]=(json.dumps(settings,indent=2).encode()+b'\n',0o600)
    changes={p:v for p,v in writes.items() if not p.exists() or p.read_bytes()!=v[0]}
    backup=None
    if changes and target.exists():
        prefix.mkdir(parents=True,exist_ok=True)
        backup=Path(tempfile.mkdtemp(prefix='upgrade-',dir=prefix)); os.chmod(backup,0o700)
        manifest=[]
        for index,p in enumerate(dict.fromkeys([target,config_path,settings_path,*changes])):
            if p.exists():
                name=f'file-{index}'
                shutil.copy2(p,backup/name)
                manifest.append({'path':str(p),'backup':name})
        (backup/'manifest.json').write_text(json.dumps(manifest,indent=2))
        url=config.get('database_url')
        if url and not url.startswith('sqlite:'):
            if not external_backup_confirmed: raise ValueError('external database requires --external-backup-confirmed after operator backup')
        else:
            if url:
                raw=url.removeprefix('sqlite://').removeprefix('sqlite:').split('?',1)[0]
                database=Path(unquote(raw))
            else: database=Path(config.get('db_path',str(prefix/'mem.db')))
            if not database.is_absolute(): raise ValueError('upgrade requires an absolute database path')
            if database.exists():
                with connection(database.resolve().as_uri()+'?mode=ro',uri=True) as original, connection(backup/'memory.db') as copied:
                    original.backup(copied)
                    if copied.execute('PRAGMA integrity_check').fetchone()[0]!='ok': raise ValueError('backup integrity check failed')
                os.chmod(backup/'memory.db',0o600)
    old={p:(p.read_bytes(),p.stat().st_mode & 0o777) if p.exists() else None for p in changes}
    installed=[]
    try:
        for p,(data,mode) in changes.items():
            atomic(p,data,mode); installed.append(p)
    except Exception:
        for p in reversed(installed):
            if old[p] is None: p.unlink()
            else: atomic(p,*old[p])
        raise
    linked=None
    link_error=None
    if link_dir is not None:
        try:
            linked=link_binary(target, link_dir)
        except OSError as exc:
            link_error=str(exc)
    instructions=path_instructions(target)
    return {'installed_binary':str(target),'changed_files':len(changes),'backup':str(backup) if backup else None,
            'mcp_configuration':str(prefix/'mcp.json'),'service_restarted':False,'hooks_installed':not no_hooks and os.name!='nt',
            'path_directory':str(target.parent),'path_instructions':instructions,'path_link':linked,'path_link_error':link_error,
            'note':'Local extraction needs no cloud key. The binary is not placed on PATH; see path_instructions. Merge mcp.json (absolute command path) into your assistant configuration. On macOS and Linux, Claude Code hooks are installed unless --no-hooks was passed. Restart running IronMem processes deliberately after verifying your backup.'}


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',type=Path,default=Path(__file__).resolve().parent.parent)
    parser.add_argument('--prefix',type=Path,default=Path.home()/'.ironmem')
    parser.add_argument('--claude-dir',type=Path,default=Path.home()/'.claude')
    parser.add_argument('--binary',type=Path)
    parser.add_argument('--no-hooks',action='store_true')
    parser.add_argument('--external-backup-confirmed',action='store_true')
    parser.add_argument('--add-to-path',nargs='?',const=str(Path.home()/'.local'/'bin'),default=None,
                        help='Symlink ironmem into DIR (default: ~/.local/bin). Does not edit shell startup files.')
    args=parser.parse_args()
    result=install(args.source,args.prefix,args.claude_dir,args.binary,args.no_hooks,args.external_backup_confirmed,args.add_to_path)
    print(json.dumps(result,indent=2))
    print(result['path_instructions'], file=sys.stderr)
    if result['hooks_installed']:
        print('Claude Code hooks were installed. Pass --no-hooks to skip them. Cursor does not use these hooks.', file=sys.stderr)
    elif os.name!='nt':
        print('Claude Code hooks were not installed.', file=sys.stderr)
    if result.get('path_link'):
        print(f'Symlinked {result["path_link"]}', file=sys.stderr)
    if result.get('path_link_error'):
        print(f'Could not symlink onto PATH: {result["path_link_error"]}', file=sys.stderr)


if __name__=='__main__': main()
