import json,sys,math,collections
def pct(v,q):
    v=sorted(v); r=min(max(math.ceil(q*len(v)),1),len(v)); return v[r-1]
d=json.load(open(sys.argv[1]))
if "resumen" in d:
    R={(f["tamano"],f["agentes"],f["calentamiento_ms"],f["variante"]):f for f in d["resumen"]}
    get=lambda k,m,q: R[k][m]["p50" if q==.5 else "p95"]
    nmu=lambda k:R[k]["muestras"]
    print("muestras",d["muestras_totales"],"fallas",len(d["fallas"]))
else:
    C=collections.defaultdict(list)
    for m in d["muestras"]: C[(m["tamano"],m["agentes"],m["calentamiento_ms"],m["variante"])].append(m)
    get=lambda k,m,q: pct([x[m] for x in C[k]],q)
    nmu=lambda k:len(C[k])
    print("muestras",len(d["muestras"]),"fallas",len(d["fallas"]), "equiv_fail",sum(1 for m in d["muestras"] if m["variante"]!="frio_blanco" and not m["estado_equivalente"]))
for metr in ("e2e_ms","carga_ms"):
  print("\n##",metr)
  tot=collections.Counter()
  for t in ("4M","64M"):
    for n in (1,4,16):
      for c in (30,300,3000):
        b=get((t,n,c,"frio_reconstruir"),metr,.5)
        row=f"{t:3} N={n:2} c={c:4} frio={b:8.1f}"
        for v in ("snap_mmap","snap_completo"):
          k=(t,n,c,v); s=get(k,metr,.5); p90=get(k,metr,.9); r=s/b
          a=s<5; bb=r<=0.10
          tot[(v,"5ms")]+=a; tot[(v,"r")]+=bb; tot[(v,"ambos")]+=a and bb
          row+=f" | {v[5:]} p50={s:6.1f} p90={p90:6.1f} n={nmu(k)} r={r:.3f} {'<5' if a else '  '}{'≤.1' if bb else '   '}"
        print(row)
  print(dict(tot))
