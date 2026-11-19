//! 网络模型：下行单队列（带宽串行化）+ 单向延迟 rtt/2 + 带 seed 的确定性抖动。全程整数微秒，不读墙钟。

use crate::trace::splitmix;

pub struct Net {
    /// 带宽，字节/秒
    bw: u64,
    rtt_us: u64,
    jitter_us: u64,
    rng: u64,
    /// 下行链路空闲时刻
    link_free_us: u64,
    last_arrive_us: u64,
}

pub struct Sent {
    /// 到达 A 的时间
    pub arrive_us: u64,
    /// 入队后发送队列里的字节数（含本帧）
    pub queue_bytes: u64,
}

impl Net {
    pub fn new(bw: u64, rtt_us: u64, jitter_us: u64, seed: u64) -> Self {
        Net { bw: bw.max(1), rtt_us, jitter_us, rng: seed, link_free_us: 0, last_arrive_us: 0 }
    }

    /// 单向延迟的一半 rtt（上行输入也用它）。
    pub fn one_way_us(&self) -> u64 { self.rtt_us / 2 }

    /// B 在 t_us 把 bytes 字节入下行队列。流内保序：到达时间不早于前一帧。
    pub fn send(&mut self, t_us: u64, bytes: u64) -> Sent {
        let backlog = self.link_free_us.saturating_sub(t_us) as u128 * self.bw as u128 / 1_000_000;
        let start = self.link_free_us.max(t_us);
        let tx = (bytes as u128 * 1_000_000 / self.bw as u128) as u64;
        self.link_free_us = start + tx;
        let jit = if self.jitter_us > 0 { splitmix(&mut self.rng) % (self.jitter_us + 1) } else { 0 };
        let arrive = (self.link_free_us + self.one_way_us() + jit).max(self.last_arrive_us);
        self.last_arrive_us = arrive;
        Sent { arrive_us: arrive, queue_bytes: backlog as u64 + bytes }
    }
}

/// 码率峰值：任意 1 秒滑动窗口（以某帧的产生时刻为窗口右端）内产生的最大字节数。输入按时间升序 (t_us, bytes)。
pub fn peak_window_bytes(sends: &[(u64, u64)]) -> u64 {
    let (mut peak, mut sum, mut lo) = (0u64, 0u64, 0usize);
    for &(t, b) in sends {
        sum += b;
        while sends[lo].0 + 1_000_000 <= t { sum -= sends[lo].1; lo += 1; }
        peak = peak.max(sum);
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(seed: u64) -> Vec<u64> {
        let mut n = Net::new(400_000, 40_000, 10_000, seed);
        (0..50).map(|i| n.send(i * 20_000, 5_000 + (i % 7) * 3_000).arrive_us).collect()
    }

    #[test]
    fn deterministic_per_seed() {
        assert_eq!(run(1), run(1));
        assert_ne!(run(1), run(2));
    }

    #[test]
    fn serialization_and_queue() {
        let mut n = Net::new(1000, 100_000, 0, 0);
        let a = n.send(0, 500);                  // 0.5 s 发完 + 50 ms
        assert_eq!(a.arrive_us, 550_000);
        let b = n.send(100_000, 500);            // 此时队列还剩 400 B
        assert_eq!(b.queue_bytes, 900);
        assert_eq!(b.arrive_us, 1_050_000);
    }

    #[test]
    fn arrival_is_monotonic_and_peak() {
        let v = run(3);
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(peak_window_bytes(&[(0, 10), (500_000, 20), (1_000_000, 5), (2_500_000, 1)]), 30);
    }
}
