const t=e=>e.replace(/[\s="]/g,"_");const l=e=>r=>{const a=t(r.label);return`${a}=${e-r.startTime}ms`};export{t as formatLabel,l as render};
