#!/usr/bin/env python3
"""Enforce the single file AppKit exception without ignoring nested Rust files."""

import pathlib
import re
import sys
import tomllib

# Remove comments/literals before looking for Rust keywords or lint overrides.
# Block comments nest in Rust; a regex for the entire comment is insufficient.
NON_CODE = re.compile(r'//[^\n]*|/\*|r(?P<hashes>\#{0,255})".*?"(?P=hashes)|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])\'', re.S)
BLOCK_EDGE = re.compile(r'/\*|\*/')
EXCEPTION = re.compile(r'#\[cfg\(target_os\s*=\s*"macos"\)\]\s*#\[allow\(unsafe_code\)\]\s*mod\s+macos\s*;')


def rust_code(source):
    result = []
    cursor = 0
    while match := NON_CODE.search(source, cursor):
        result.append(source[cursor:match.start()])
        end = match.end()
        if match.group() == '/*':
            depth = 1
            while depth:
                edge = BLOCK_EDGE.search(source, end)
                if edge is None:
                    end = len(source)
                    break
                depth += 1 if edge.group() == '/*' else -1
                end = edge.end()
        result.append(' ' * (end - match.start()))
        cursor = end
    result.append(source[cursor:])
    return ''.join(result)


def check(root):
    workspace = tomllib.loads((root / 'Cargo.toml').read_text())
    errors = []
    if workspace['workspace']['lints']['rust'].get('unsafe_code') != 'forbid':
        errors.append('workspace must keep unsafe_code = forbid')
    for member in workspace['workspace']['members']:
        manifest = tomllib.loads((root / member / 'Cargo.toml').read_text())
        if member == 'crates/native-menu':
            if manifest.get('lints', {}).get('rust', {}).get('unsafe_code') != 'deny':
                errors.append('native-menu must default to unsafe_code = deny')
            if manifest.get('lib', {}).get('path') != 'src/native_menu.rs':
                errors.append('native-menu must retain its checked library root')
        elif manifest.get('lints', {}).get('workspace') is not True:
            errors.append(f'{member} must inherit workspace unsafe forbid')
        if 'gpui-component' in manifest.get('dependencies', {}):
            errors.append(f'{member} must not restore Component')
    source_root = root / 'crates/native-menu/src'
    library = source_root / 'native_menu.rs'
    allowed = source_root / 'macos.rs'
    if source_root.is_symlink():
        errors.append('native-menu/src must not redirect the checked source boundary')
    if not allowed.is_file():
        errors.append('the approved src/macos.rs must exist')
    for node in source_root.rglob('*'):
        if node.is_symlink():
            errors.append(f'{node} must not redirect the checked source boundary')
    for source in source_root.rglob('*.rs'):
        if source.is_symlink():
            continue
        text = source.read_text()
        if source == library:
            matches = list(EXCEPTION.finditer(text))
            if len(matches) != 1:
                errors.append('only the macOS-gated mod macos declaration may allow unsafe_code')
            else:
                match = matches[0]
                text = text[:match.start()] + ' ' * len(match.group()) + text[match.end():]
        code = rust_code(text)
        if re.search(r'\bunsafe_code\b', code):
            errors.append(f'{source} contains an unauthorized unsafe_code lint override')
        if source != allowed and re.search(r'\bunsafe\b', code):
            errors.append(f'FFI escaped src/macos.rs: {source}')
        attributes = re.findall(r'#\s*!?\s*\[(.*?)\]', code, re.S)
        if re.search(r'\binclude\s*!\s*\(', code) or any(re.search(r'\bpath\s*=', attribute) for attribute in attributes):
            errors.append(f'{source} imports Rust outside the checked module paths')
    return errors


if __name__ == '__main__':
    failures = check(pathlib.Path(__file__).resolve().parents[1])
    for failure in failures:
        print(f'architecture violation: {failure}', file=sys.stderr)
    sys.exit(bool(failures))
