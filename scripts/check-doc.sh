#!/usr/bin/env bash
# 检查架构文档的完整性。用于防止「插入内容时覆盖邻近章节」与「引用指向不存在的章节」这类事故。
set -uo pipefail

DOC="${1:-docs/architecture.md}"
fail=0

if [ ! -f "$DOC" ]; then
    echo "找不到 $DOC"
    exit 1
fi

echo "=== 章节连续性 ==="
nums=$(grep -oE '^## [0-9]+\.' "$DOC" | grep -oE '[0-9]+' | sort -n)
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
for a in A B C; do
    if grep -q "^## 附录 ${a}" "$DOC"; then
        echo "附录 ${a} 存在"
    else
        echo "附录 ${a} 缺失"
        fail=1
    fi
done

echo "=== 交叉引用 ==="
refs=$(grep -oE '§[0-9]+' "$DOC" | sed 's/§//' | sort -n -u)
dangling=""
for n in $refs; do
    if ! echo "$nums" | grep -qx "$n"; then
        dangling="$dangling §$n"
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
fences=$(grep -c '^```' "$DOC" || echo 0)
if [ $((fences % 2)) -ne 0 ]; then
    echo "围栏数为奇数（${fences}），有未闭合的代码块"
    fail=1
else
    echo "围栏 ${fences} 个，成对"
fi

echo "=== 占位符 ==="
if grep -q '@@@' "$DOC"; then
    echo "发现未替换的 @@@ 占位符"
    fail=1
else
    echo "无残留占位符"
fi

echo "=== 规模 ==="
lines=$(wc -l < "$DOC" | tr -d ' ')
if [ "$lines" -lt 1500 ]; then
    echo "行数 ${lines} 低于预期下限 1500，可能被截断"
    fail=1
else
    echo "行数 ${lines}"
fi

echo ""
if [ "$fail" -eq 0 ]; then
    echo "文档完整性检查通过"
else
    echo "文档完整性检查失败"
fi
exit "$fail"
