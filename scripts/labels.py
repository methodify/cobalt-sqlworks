import sys, json
d = json.load(sys.stdin)
out = []
def walk(n):
    if isinstance(n, dict):
        if n.get('label'):
            out.append((n.get('role'), n.get('label')[:70]))
        for key in ('children', 'nodes'):
            for c in n.get(key, []) or []:
                walk(c)
    elif isinstance(n, list):
        for c in n:
            walk(c)
walk(d.get('snapshot', d))
print('labeled nodes:', len(out))
roles = sys.argv[1:] or ['button', 'checkbox', 'textbox', 'tab', 'menuitem', 'combobox']
print([l for r, l in out if r in roles][:120])
