# STC8G1K08A SOP8 Sine-Like PWM Example

This example targets the `STC8G1K08A` in `SOP8` package and uses `P3.3` as the PWM output pin.

## Why this uses software PWM

The hardware `PCA/CCP/PWM` outputs are available only on packages that expose `P1.x` or `P3.5~P3.7`.
The `SOP8` package only exposes `P3.0~P3.3`, `P5.4`, and `P5.5`, so this version uses software PWM.
The default output pin is `P3.3` instead of `P3.2` to avoid conflicts with USB direct-download wiring.

Official datasheet:
<https://www.stcmicro.com/datasheet/stc8g1k08.pdf>

## Current settings

- PWM output pin: `P3.3`
- Software PWM resolution: `100` steps
- PWM frequency: about `500Hz`
- Duty cycle range: `10% ~ 100%`
- Full sine-like period: about `1.024s`

## How to tune it

- Change output pin: edit `PWM_OUT = P3 ^ 3` in [main.c](/q:/609/dpj/main.c)
- Change PWM frequency: edit `PWM_TICK_US` or `PWM_STEPS`
- Change sine speed: edit `SINE_HOLD_PWM_CYCLES`
- Change duty range: edit `g_sine_duty_table`

## Power-on behavior

As soon as `main()` runs, the code initializes the clock, port mode, and `Timer2`, then starts PWM output on `P3.3`.
