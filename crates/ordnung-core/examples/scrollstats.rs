//! Experiment harness: compare column reducers against golden PWV7 stats.
struct Bq { b0: f32, b1: f32, b2: f32, a1: f32, a2: f32, x1: f32, x2: f32, y1: f32, y2: f32 }
impl Bq {
    fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0/a0, b1: b1/a0, b2: b2/a0, a1: a1/a0, a2: a2/a0, x1:0., x2:0., y1:0., y2:0. }
    }
    fn lp(hz: f32, sr: u32) -> Self {
        let w = 2.0*std::f32::consts::PI*hz/sr as f32; let (s,c)=w.sin_cos(); let al=s/std::f32::consts::SQRT_2;
        let b1=1.0-c; Self::new(b1/2.0,b1,b1/2.0,1.0+al,-2.0*c,1.0-al)
    }
    fn hp(hz: f32, sr: u32) -> Self {
        let w = 2.0*std::f32::consts::PI*hz/sr as f32; let (s,c)=w.sin_cos(); let al=s/std::f32::consts::SQRT_2;
        let b1=1.0+c; Self::new(b1/2.0,-b1,b1/2.0,1.0+al,-2.0*c,1.0-al)
    }
    #[inline] fn step(&mut self, x: f32) -> f32 {
        let y = self.b0*x + self.b1*self.x1 + self.b2*self.x2 - self.a1*self.y1 - self.a2*self.y2;
        self.x2=self.x1; self.x1=x; self.y2=self.y1; self.y1=y; y
    }
}
fn pctf(v: &mut Vec<f32>, p: f64) -> f32 { v.sort_by(|a,b| a.partial_cmp(b).unwrap()); v[((v.len()-1) as f64 * p) as usize] }
fn main() {
    let path = std::env::args().nth(1).expect("audio path");
    let a = ordnung_core::analysis::decode_mono(&path).expect("decode");
    let sr = a.sample_rate.max(1) as u64;
    let n = ((a.samples.len() as u64 * 150) / sr).max(1) as usize;
    // Per-column peak + sum-of-squares for each band.
    let mut lp = Bq::lp(200.0, a.sample_rate);
    let mut mh = Bq::hp(200.0, a.sample_rate); let mut ml = Bq::lp(2000.0, a.sample_rate);
    let mut hp = Bq::hp(2000.0, a.sample_rate);
    let mut peak = vec![[0f32;3]; n]; let mut ssq = vec![[0f64;3]; n]; let mut cnt = vec![0u32; n];
    for (i,&s) in a.samples.iter().enumerate() {
        let col = ((i as u64 * 150)/sr) as usize; let col = col.min(n-1);
        let v = [lp.step(s).abs(), ml.step(mh.step(s)).abs(), hp.step(s).abs()];
        for k in 0..3 { peak[col][k]=peak[col][k].max(v[k]); ssq[col][k]+= (v[k]*v[k]) as f64; }
        cnt[col]+=1;
    }
    let rms: Vec<[f32;3]> = (0..n).map(|i| {
        let c = cnt[i].max(1) as f64; [ (ssq[i][0]/c).sqrt() as f32, (ssq[i][1]/c).sqrt() as f32, (ssq[i][2]/c).sqrt() as f32 ]
    }).collect();
    // Windowed RMS over +/- w columns (power-averaged).
    let wrms = |w: usize| -> Vec<[f32;3]> {
        (0..n).map(|i| {
            let lo = i.saturating_sub(w); let hi = (i+w+1).min(n);
            let mut acc=[0f64;3]; let mut c=0u32;
            for j in lo..hi { for k in 0..3 { acc[k]+=(rms[j][k]*rms[j][k]) as f64; } c+=1; }
            [ (acc[0]/c as f64).sqrt() as f32, (acc[1]/c as f64).sqrt() as f32, (acc[2]/c as f64).sqrt() as f32 ]
        }).collect()
    };
    // The real library implementation, for comparison.
    let real = ordnung_core::analysis::waveform::scroll_bands(&a.samples, a.sample_rate);
    let rn = real.len()/4;
    println!("== library scroll_bands");
    for k in 0..3 {
        let b: Vec<f32> = real.iter().skip(k).step_by(4).map(|&v| v as f32).collect();
        let mean = b.iter().sum::<f32>()/rn as f32;
        let mut bb = b.clone();
        println!("  band{k}: mean={mean:5.1} p5={:5.1} med={:5.1} p95={:5.1} max={:5.1}", pctf(&mut bb.clone(),0.05), pctf(&mut bb.clone(),0.5), pctf(&mut bb.clone(),0.95), pctf(&mut bb,1.0));
    }
    let golden = [[43.,12.,32.,109.,126.],[17.,2.,11.,55.,126.],[4.,0.,0.,16.,127.]];
    for (label, cols) in [("peak", peak.clone()), ("rms", rms.clone()), ("rms_w2", wrms(2)), ("rms_w4", wrms(4))] {
        // Normalize by the global max across bands, scale to 127.
        let gmax = cols.iter().flat_map(|c| c.iter().copied()).fold(0f32, f32::max).max(1e-6);
        println!("== {label} (norm by band max {gmax:.3})");
        for k in 0..3 {
            let mut b: Vec<f32> = cols.iter().map(|c| c[k]*127.0/gmax).collect();
            let mean = b.iter().sum::<f32>()/n as f32;
            let (p5,med,p95,max)=(pctf(&mut b.clone(),0.05),pctf(&mut b.clone(),0.5),pctf(&mut b.clone(),0.95),pctf(&mut b,1.0));
            let g=&golden[k];
            println!("  band{k}: mean={mean:5.1} p5={p5:5.1} med={med:5.1} p95={p95:5.1} max={max:5.1}   golden: mean={} p5={} med={} p95={} max={}", g[0],g[1],g[2],g[3],g[4]);
        }
    }
}
