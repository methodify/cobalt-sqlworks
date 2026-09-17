import sys, json, base64
d = json.load(sys.stdin)
img = d.get('image') or d.get('result', {}).get('image') or d
open(sys.argv[1], 'wb').write(base64.b64decode(img['data']))
print('saved', sys.argv[1], img.get('width'), img.get('height'))
