use chrono::Utc;
use crossbeam::channel::{Receiver, bounded};
use ndarray::{Array1, Axis, s};
use num::Complex;
use rsdsp::{ospfb2::Analyzer, windowed_fir::pfb_coeff};
use soapysdr::RxStream;

type Ftype = f32;

pub fn run_daq(
    mut sdr_stream: RxStream<Complex<Ftype>>,
    nch: usize,
    tap_per_ch: usize,
    n_average: usize,
) -> Receiver<Array1<f32>> {
    match sdr_stream.activate(None) {
        Ok(()) => {
            println!("activated")
        }
        Err(e) => {
            println!("{e:?}");
        }
    }

    let coeff = pfb_coeff::<Ftype>(nch / 2, tap_per_ch, 1.1 as Ftype);
    let mut pfb = Analyzer::<Complex<Ftype>, Ftype>::new(nch, coeff.as_slice().unwrap());

    let (tx_raw, rx_raw) = bounded(64);

    let mut num = 0;
    let mut cnt = 0;

    std::thread::spawn(move || {
        let t0 = Utc::now().timestamp_millis(); // e.g. `2014-11-28T12:45:59.324310806Z`
        let mut sigma = None;
        loop {
            let mut buf = Vec::with_capacity(sdr_stream.mtu().unwrap());
            buf.resize(sdr_stream.mtu().unwrap(), Complex::default());
            let len = sdr_stream
                .read(&mut [&mut buf], 1_000_000)
                .expect("read failed");
            buf.resize(len, Complex::default());
            let sigma1 = buf
                .iter()
                .map(|x| x.norm_sqr())
                .reduce(|a, b| a + b)
                .unwrap()
                / buf.len() as f32;
            let k = 0.999;
            if let Some(ref mut x) = sigma {
                *x = *x * k + (1.0 - k) * sigma1;
            } else {
                sigma = Some(sigma1);
            }

            if !tx_raw.is_full() && tx_raw.send(buf).is_err() {
                break;
            }

            cnt += 1;
            num += len as i64;
            if cnt % 100 == 0 {
                let t1 = Utc::now().timestamp_millis();
                let dt_sec = (t1 - t0) as f64 / 1000.0;
                let sps = num as f64 / dt_sec;
                println!(
                    "{} Msps Q={} pwr={} dB",
                    sps / 1e6,
                    tx_raw.len(),
                    sigma.unwrap_or(1e-30).log10() * 10.0
                );
            }
        }
    });

    let (tx_spectrum, rx_spectrum) = bounded(n_average * 2);

    std::thread::spawn(move || {
        loop {
            let data: Vec<Complex<f32>> = rx_raw.recv().unwrap();
            let analyzed = pfb.analyze_raw_par(&data);
            let mut receiver_disconnected = false;
            for x in analyzed.axis_iter(Axis(0)) {
                let x1 = Array1::from_iter(
                    x.slice(s![nch / 2..nch])
                        .iter()
                        .chain(x.slice(s![0..nch / 2]))
                        .map(|x1| x1.norm_sqr()),
                );
                if !tx_spectrum.is_full() {
                    if tx_spectrum.send(x1).is_err() {
                        receiver_disconnected = true;
                        break;
                    }
                } else {
                    println!("WARNING: spectrum queue is full, skipping");
                }
            }
            if receiver_disconnected {
                break;
            }
        }
    });

    let (tx_averaged, rx_averaged) = bounded(16);

    std::thread::spawn(move || {
        loop {
            let mut temp = Array1::<Ftype>::zeros(nch);
            for _i in 0..n_average {
                temp = temp + rx_spectrum.recv().unwrap();
            }
            temp /= n_average as Ftype;

            if !tx_averaged.is_full() && temp.iter().all(|&x| x > 0_f32) {
                if tx_averaged.send(temp).is_err() {
                    break;
                }
            } else {
                println!("average data queue full, skipping");
            }
        }
    });

    rx_averaged
}
