from pathlib import Path
import json
import sys
if len(sys.argv) != 3:
    raise SystemExit('usage: derive_unicode.py UnicodeData-3.0.0.txt SpecialCasing-2.txt')
rows=[line.split(';') for line in Path(sys.argv[1]).read_text().splitlines()]
fold={}; ident=set([0x24,0x5f]); spaces=set([9,10,11,12,13,0x20,0xa0,0x2028,0x2029,0xfeff])
start=None
for r in rows:
    u=int(r[0],16); cat=r[2]
    if r[1].endswith(', First>'): start=u; continue
    if r[1].endswith(', Last>'):
        assert start is not None
        units=range(start,u+1)
    else:
        units=[u]
    start=None
    if cat in ['Lu','Ll','Lt','Lm','Lo','Nl','Mn','Mc','Nd','Pc']: ident.update(x for x in units if x<=65535)
    if cat=='Zs': spaces.update(units)
    if r[12] and u<=65535:
        upper=int(r[12],16)
        if upper!=u and not(u>=128 and upper<128):fold[u]=upper
for line in Path(sys.argv[2]).read_text().splitlines():
    line=line.split('#')[0].strip()
    if not line:continue
    cols=[x.strip() for x in line.split(';')]
    if len(cols)>4 and cols[4]:continue
    u=int(cols[0],16);upper=[int(x,16) for x in cols[3].split()]
    if len(upper)!=1 or upper[0]>65535 or(u>=128 and upper[0]<128):fold.pop(u,None)
    elif upper[0]!=u:fold[u]=upper[0]
def ranges(s):
    out=[]
    for x in sorted(s):
        if out and out[-1][1]+1==x:out[-1][1]=x
        else:out.append([x,x])
    return out
lines=['# Derived fixed Unicode 3.0 tables for ECMA-262 5.1 sections 6, 7.2, 7.6, 15.10.2.8.', '# Sources: https://www.unicode.org/Public/3.0-Update/UnicodeData-3.0.0.txt', '# https://www.unicode.org/Public/3.0-Update/SpecialCasing-2.txt', '# U: ES5 Canonicalize nonidentity pair; I: IdentifierPart range; S: WhiteSpace/LineTerminator range.', '# Default unconditional uppercase only; multi-unit/non-ASCII-to-ASCII uppercase excluded.', '# Unicode data copyright Unicode, Inc.; https://www.unicode.org/license.txt']
lines += [f'U {a:04X} {b:04X}' for a,b in sorted(fold.items())]
for kind,items in [('I',ident),('S',spaces)]:lines += [f'{kind} {a:04X} {b:04X}' for a,b in ranges(items)]
print('\n'.join(lines))
