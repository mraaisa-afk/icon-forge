import re,sys
# Count positional `{}` placeholders in each format string and compare with the
# macro's argument count. Slips of this kind only show up in CI otherwise.
src=open(sys.argv[1]).read()
i=0; bad=0; checked=0
while True:
    m=re.compile(r'\b(eprintln|println|panic|assert|assert_eq|assert_ne|format|write|writeln)!\s*\(').search(src,i)
    if not m: break
    j=m.end(); depth=1; k=j
    while depth:
        c=src[k]
        if c=='(' : depth+=1
        elif c==')': depth-=1
        elif c=='"' and src[k-1] != '\\':
            k+=1
            while not (src[k]=='"' and src[k-1]!='\\'): k+=1
        elif c=="'":  # char or lifetime; skip to next quote
            k+=1
            while src[k]!="'": k+=1
        k+=1
    body=src[j:k-1]
    i=k
    # split top-level commas
    args=[]; depth=0; cur=''; k=0
    while k<len(body):
        c=body[k]
        if c=='"' and body[k-1]!='\\':
            cur+=c; k+=1
            while not (body[k]=='"' and body[k-1]!='\\'): cur+=body[k]; k+=1
            cur+=body[k]
        elif c in '([{': depth+=1; cur+=c
        elif c in ')]}': depth-=1; cur+=c
        elif c==',' and depth==0: args.append(cur); cur=''
        else: cur+=c
        k+=1
    if cur.strip(): args.append(cur)
    # find the literal that is a format string: the first arg that starts with a quote
    fi=None
    for n,a in enumerate(args):
        if a.strip().startswith('"') or a.strip().startswith('r"'): fi=n; break
    if fi is None: continue
    lit=args[fi].strip()
    # concatenate implicit line-continuation string-literal fragments
    checked+=1
    # count positional placeholders: {} with digits/format spec, not {name}, not {{
    # strip escaped braces and named args first
    t=lit.replace('{{','').replace('}}','')
    # a placeholder is positional unless it names an argument inline ({name} or
    # {name:.3}) — those resolve against variables in scope, not the arg list.
    npos=0
    for inner in re.findall(r'\{([^{}]*)\}', t):
        if re.match(r'^[A-Za-z_][A-Za-z0-9_]*(:|$)', inner):
            continue
        npos+=1
    # inline named uses referencing args? e.g. {x:.3} with `x` bound in scope. Assume in scope.
    nargs=len(args)-(fi+1)
    if npos!=nargs:
        line=src[:m.start()].count('\n')+1
        print(f'{sys.argv[1]}:{line}: {m.group(1)}!: {npos} positional placeholders vs {nargs} args')
        print('    fmt:',lit[:90].replace('\n',' '))
        print('    args:',[a.strip()[:40] for a in args[fi+1:]])
        bad+=1
print(f'checked {checked} format macros, {bad} mismatch(es)')
