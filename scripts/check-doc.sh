#!/usr/bin/env bash
# 检查架构文档的完整性。默认检查总目录 + 全部分册；也可传单个文件。
# 用于防止「插入内容时覆盖邻近章节」与「引用指向不存在的章节」这类事故。
set -uo pipefail

if [ "$#" -gt 0 ]; then
    FILES="$1"
else
    FILES="docs/architecture.md"
    if [ -d docs/architecture ]; then
        FILES="$FILES $(ls docs/architecture/*.md 2>/dev/null | sort)"
    fi
fi

for f in $FILES; do
    if [ ! -f "$f" ]; then
        echo "找不到 $f"
        exit 1
    fi
done

tmp=$(mktemp "${TMPDIR:-/tmp}/toolgate-doc-check.XXXXXX")
trap 'rm -f "$tmp"' EXIT
cat $FILES > "$tmp"

fail=0

echo "=== 文件 ==="
for f in $FILES; do
    echo "  $f"
done

echo "=== 章节连续性 ==="
nums=$(grep -oE '^## [0-9]+\.' "$tmp" | grep -oE '[0-9]+' | sort -n)
if [ -z "$nums" ]; then
    echo "没有找到任何编号章节"
    fail=1
    max=0
else
    max=$(echo "$nums" | tail -1)
    missing=""
    for n in $(seq 0 "$max"); do
        if ! echo "$nums" | grep -qx "$n"; then
            missing="$missing $n"
        fi
    done
    if [ -n "$missing" ]; then
        echo "缺失章节:$missing（最大章节号 $max）"
        fail=1
    else
        echo "§0-$max 全部存在"
    fi
fi

echo "=== 附录 ==="
for a in A B C D; do
    if grep -q "^## 附录 ${a}" "$tmp"; then
        echo "附录 ${a} 存在"
    else
        echo "附录 ${a} 缺失"
        fail=1
    fi
done

echo "=== 交叉引用 ==="
refs=$(grep -oE '§[0-9]+(\.[0-9]+){0,2}[a-z]?' "$tmp" | sed 's/§//' | sort -u)
dangling=""
for r in $refs; do
    base=$(echo "$r" | grep -oE '^[0-9]+')
    if ! echo "$nums" | grep -qx "$base"; then
        dangling="$dangling §$r"
        continue
    fi
    if echo "$r" | grep -qE '\.'; then
        esc=$(echo "$r" | sed 's/\./\\./g')
        if ! grep -qE "^#{2,4} ${esc}([^0-9a-z]|$)" "$tmp"; then
            dangling="$dangling §$r"
        fi
    fi
done
if [ -n "$dangling" ]; then
    echo "悬空引用:$dangling"
    fail=1
else
    count=$(echo "$refs" | wc -l | tr -d ' ')
    echo "交叉引用全部存在（${count} 个不同章节号）"
fi

echo "=== 代码围栏 ==="
for f in $FILES; do
    fences=$(grep -c '^```' "$f" || echo 0)
    if [ $((fences % 2)) -ne 0 ]; then
        echo "$f 围栏数为奇数（${fences}），有未闭合的代码块"
        fail=1
    fi
done
if [ "$fail" -eq 0 ]; then
    echo "各文件围栏均成对"
fi

echo "=== 占位符 ==="
if grep -q '@@@' "$tmp"; then
    echo "发现未替换的 @@@ 占位符"
    fail=1
else
    echo "无残留占位符"
fi

echo "=== 内部链接 ==="
if ! python3 scripts/check-links.py; then
    fail=1
fi

echo "=== 规模 ==="
lines=$(wc -l < "$tmp" | tr -d ' ')
if [ "$lines" -lt 1500 ]; then
    echo "总行数 ${lines} 低于预期下限 1500，可能被截断"
    fail=1
else
    echo "总行数 ${lines}"
fi

echo ""
if [ "$fail" -eq 0 ]; then
    echo "文档完整性检查通过"
else
    echo "文档完整性检查失败"
fi
exit "$fail"
