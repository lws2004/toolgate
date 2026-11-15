#!/usr/bin/env python3
"""P1（rerank 预算按字节）与 P2（daemon 冷启预热）实测。用法：python3 reports/bench-p13-2026-11-15.py <socket> [--wait-socket]"""
import http.client, json, socket, sys, time

def call(sock_path, method, path, payload=None, timeout=180):
    body = b'' if payload is None else json.dumps(payload, ensure_ascii=False).encode()
    conn = http.client.HTTPConnection('localhost', timeout=timeout)
    conn.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    conn.sock.connect(sock_path)
    t = time.perf_counter()
    conn.request(method, path, body=body, headers={'content-type': 'application/json'})
    r = conn.getresponse(); text = r.read().decode(); el = time.perf_counter() - t; conn.close()
    return r.status, text, el

def govern(sock, tool, text, intent='raw', extra=None):
    req = {'source': {'kind': 'tool', 'tool': tool}, 'content': [{'type': 'text', 'text': text}], 'intent': intent, 'policy': {'isolation': 'summary'}}
    if extra: req.update(extra)
    st, tx, el = call(sock, 'POST', '/govern', req)
    if st != 200: return {'status': st, 'error': tx[:200], 'ms': round(el*1000)}
    o = json.loads(tx); sc = o.get('structuredContent') or {}; meta = o.get('meta') or {}
    out = ''.join(b.get('text','') for b in o.get('content') or [] if b.get('type') == 'text')
    return {'status': st, 'ms': round(el*1000), 'shape': sc.get('shape'), 'truncated': sc.get('truncated'),
            'queryApplied': sc.get('queryApplied'), 'chunked': sc.get('chunked'), 'slmStatus': meta.get('slmStatus'),
            'bytesOut': len(out.encode()), 'notices': [n.get('code') for n in sc.get('notices') or []],
            'hasRANKED': 'RANKED' in out, 'hasByteClamp': ('字节上限截断' in out)}

def wait_socket(sock, deadline=90):
    t0 = time.time()
    while time.time() - t0 < deadline:
        try:
            st, _, _ = call(sock, 'GET', '/health', None, timeout=3)
            if st == 200: return round((time.time()-t0)*1000)
        except Exception: pass
        time.sleep(0.05)
    return -1

def make_unique_long(lines, width):
    return '\n'.join(f'2026-11-15 10:00:00 INFO unique task {i} ' + 'z'*width for i in range(lines))

def main():
    sock = sys.argv[1]
    if '--wait-socket' in sys.argv:
        ms = wait_socket(sock); print(f'# socket 就绪耗时 {ms}ms')
    big = make_unique_long(4000, 0)
    print(f'# 大请求字节 {len(big.encode())}')
    print('COLD', json.dumps(govern(sock, 'read', big, 'summary'), ensure_ascii=False))
    print('WARM', json.dumps(govern(sock, 'read', big, 'summary'), ensure_ascii=False))
    wide = '\n'.join(f'- item {i} src/lib.rs:{i}:1: ' + 'y'*300 for i in range(120))
    print(f'# 列表字节 {len(wide.encode())}')
    print('P1_39K_NOQUERY', json.dumps(govern(sock, 'search', wide, 'raw'), ensure_ascii=False))
    short = '\n'.join(f'- item {i} src/lib.rs:{i}:1: migration failed' for i in range(120))
    print('P1_120_WITHQUERY', json.dumps(govern(sock, 'search', short, 'raw', {'query': '数据库迁移失败的原因'}), ensure_ascii=False))
    reg = make_unique_long(600, 54)
    print('REG_600_SUMMARY', json.dumps(govern(sock, 'read', reg, 'summary'), ensure_ascii=False))

if __name__ == '__main__': main()