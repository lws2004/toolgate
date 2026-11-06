#!/usr/bin/env python3
"""检查 Markdown 内部链接：目标文件存在、锚点匹配。"""
import os
import re
import sys


def slug(title: str) -> str:
    s = title.strip().lower()
    s = re.sub(r'[^\w\u4e00-\u9fff\- ]', '', s)
    return s.replace(' ', '-')


def md_files(root: str = '.'):
    for directory, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if not d.startswith('.') and d not in ('target', 'node_modules')]
        for name in files:
            if name.endswith('.md'):
                yield os.path.join(directory, name)


pattern = re.compile(r'\[([^\]]*)\]\(([^)]+)\)')
bad = 0
for path in sorted(md_files()):
    text = open(path, encoding='utf-8').read()
    for line_no, line in enumerate(text.splitlines(), 1):
        for match in pattern.finditer(line):
            target = match.group(2).strip()
            if target.startswith(('http://', 'https://', 'mailto:', '#')):
                continue
            file_part, _, anchor = target.partition('#')
            resolved = os.path.normpath(os.path.join(os.path.dirname(path), file_part)) if file_part else path
            if not os.path.exists(resolved):
                print(f'{path}:{line_no}: 文件不存在 {target}')
                bad += 1
                continue
            if anchor:
                headings = [l.lstrip('#').strip() for l in open(resolved, encoding='utf-8').read().splitlines() if l.startswith('#')]
                if anchor not in [slug(h) for h in headings]:
                    print(f'{path}:{line_no}: 锚点不存在 {target}')
                    bad += 1

if bad:
    print(f'链接检查失败：{bad} 处问题')
    sys.exit(1)
print('链接检查通过')
