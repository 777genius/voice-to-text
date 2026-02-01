use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

/// Быстрый анализатор спектра для UI-визуализатора.
///
/// Дизайн:
/// - Вход: i16 PCM, 16kHz mono (как у нас после resample/VAD)
/// - FFT: 256 точек → 128 бинов
/// - Выход: 48 значений 0..1 (готовые "бары" для canvas)
///
/// Важно:
/// - Это не аудио-процессинг для STT, а чисто визуальный эффект.
/// - Стараемся не аллоцировать на каждом кадре, чтобы не создавать лишнюю нагрузку.
pub struct AudioSpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: [f32; FFT_SIZE],
    ring: [f32; FFT_SIZE],
    write_pos: usize,
    filled: usize,
    fft_buf: [Complex<f32>; FFT_SIZE],
    bars: [f32; BAR_COUNT],
}

const FFT_SIZE: usize = 256;
const BAR_COUNT: usize = 48;

impl AudioSpectrumAnalyzer {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);

        // Hann window - делает картинку более стабильной и приятной
        let mut window = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            window[i] = 0.5 - 0.5 * ((2.0 * std::f32::consts::PI * i as f32) / (FFT_SIZE as f32 - 1.0)).cos();
        }

        Self {
            fft,
            window,
            ring: [0.0; FFT_SIZE],
            write_pos: 0,
            filled: 0,
            fft_buf: [Complex { re: 0.0, im: 0.0 }; FFT_SIZE],
            bars: [0.0; BAR_COUNT],
        }
    }

    /// Добавляет новые сэмплы в ring-buffer и, если данных достаточно, возвращает свежий спектр.
    ///
    /// Возвращаем сразу 48 баров (0..1), чтобы фронту было проще и дешевле рисовать.
    pub fn push_samples(&mut self, samples: &[i16]) -> Option<[f32; BAR_COUNT]> {
        for &s in samples {
            // Нормализуем в -1..1
            let v = (s as f32 / 32767.0).clamp(-1.0, 1.0);
            self.ring[self.write_pos] = v;
            self.write_pos = (self.write_pos + 1) % FFT_SIZE;
            self.filled = self.filled.saturating_add(1).min(FFT_SIZE);
        }

        if self.filled < FFT_SIZE {
            return None;
        }

        // Формируем буфер в правильном порядке (самые старые → самые новые)
        for i in 0..FFT_SIZE {
            let src = self.ring[(self.write_pos + i) % FFT_SIZE] * self.window[i];
            self.fft_buf[i].re = src;
            self.fft_buf[i].im = 0.0;
        }

        self.fft.process(&mut self.fft_buf);

        // 0..127 (половина спектра)
        let max_bin = (FFT_SIZE / 2) - 1; // 127
        let min_bin = 1; // пропускаем DC
        let bins = &self.fft_buf[..=max_bin];

        // Чуть "музыкальнее" распределяем бины по барам (лог-шкала)
        let min_f = min_bin as f32;
        let max_f = max_bin as f32;
        let ratio = max_f / min_f;

        for bar in 0..BAR_COUNT {
            let a = min_f * ratio.powf(bar as f32 / BAR_COUNT as f32);
            let b = min_f * ratio.powf((bar + 1) as f32 / BAR_COUNT as f32);

            let mut start = a.floor() as usize;
            let mut end = b.ceil() as usize;
            start = start.clamp(min_bin, max_bin);
            end = end.clamp(start + 1, max_bin + 1);

            let mut sum = 0.0f32;
            let mut count = 0.0f32;

            for bin in start..end {
                let c = bins[bin];
                let mag = (c.re * c.re + c.im * c.im).sqrt();
                // Нормализация на размер FFT, иначе значения слишком большие
                sum += mag / FFT_SIZE as f32;
                count += 1.0;
            }

            let avg = if count > 0.0 { sum / count } else { 0.0 };

            // Компрессия, чтобы речь выглядела "живее" и не была еле заметной
            // ln1p хорошо работает и не дергается на малых значениях.
            let scaled = (avg * 80.0).ln_1p() / (80.0f32).ln_1p();
            self.bars[bar] = scaled.clamp(0.0, 1.0);
        }

        Some(self.bars)
    }
}

