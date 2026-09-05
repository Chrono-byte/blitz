const o=`effect/GlobalValue`;let l;const t=(e,a)=>{if(!l){globalThis[o]??=new Map;l=globalThis[o]}if(!l.has(e)){l.set(e,a())}return l.get(e)};export{t as globalValue};
