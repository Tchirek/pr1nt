#include <reg51.h>

#define CLKDIV (*(unsigned char volatile xdata *)0xfe01)

#define FOSC                 24000000UL
#define PWM_TICK_US          20U
#define PWM_STEPS            100U
#define SINE_SAMPLES         64U
#define SINE_HOLD_PWM_CYCLES 8U
#define T2_RELOAD            (65536UL - (FOSC / 12UL * PWM_TICK_US / 1000000UL))

#define ET2  0x04
#define T2IF 0x01

sfr P_SW2 = 0xba;
sfr IRCCR = 0x9f;
sfr AUXR = 0x8e;
sfr IE2 = 0xaf;
sfr T2H = 0xd6;
sfr T2L = 0xd7;
sfr AUXINTIF = 0xef;
sfr P3M1 = 0xb1;
sfr P3M0 = 0xb2;

sbit PWM_OUT = P3 ^ 3; /* PWM output on SOP8 */

static unsigned char g_pwm_counter;
static unsigned char g_pwm_duty;
static unsigned char g_sine_index;
static unsigned char g_sine_hold;

static unsigned char code g_sine_duty_table[SINE_SAMPLES] = {
    55, 59, 64, 68, 72, 76, 80, 84,
    87, 90, 92, 95, 97, 98, 99, 100,
    100, 100, 99, 98, 97, 95, 92, 90,
    87, 84, 80, 76, 72, 68, 64, 59,
    55, 51, 46, 42, 38, 34, 30, 26,
    23, 20, 18, 15, 13, 12, 11, 10,
    10, 10, 11, 12, 13, 15, 18, 20,
    23, 26, 30, 34, 38, 42, 46, 51
};

void Clock_Init24M(void)
{
    unsigned char idata *irc24;

    irc24 = (unsigned char idata *)0xfb;
    IRCCR = *irc24; /* Load factory-calibrated 24MHz IRC trim */

    P_SW2 |= 0x80;
    CLKDIV = 0x00; /* No system clock divider */
    P_SW2 &= ~0x80;
}

void Port_Init(void)
{
    P3M1 &= ~0x08;
    P3M0 |= 0x08; /* P3.3 push-pull output */
    PWM_OUT = 1;
}

void Timer2_Init(void)
{
    T2H = (unsigned char)(T2_RELOAD >> 8);
    T2L = (unsigned char)T2_RELOAD;

    AUXINTIF &= ~T2IF;
    IE2 |= ET2;

    AUXR &= ~0x04; /* Timer2 in 12T mode */
    AUXR |= 0x10;  /* Start Timer2 */
}

void Timer2_Isr(void) interrupt 12
{
    AUXINTIF &= ~T2IF;

    if (g_pwm_counter < g_pwm_duty)
    {
        PWM_OUT = 1;
    }
    else
    {
        PWM_OUT = 0;
    }

    g_pwm_counter++;
    if (g_pwm_counter >= PWM_STEPS)
    {
        g_pwm_counter = 0;
        g_sine_hold++;

        if (g_sine_hold >= SINE_HOLD_PWM_CYCLES)
        {
            g_sine_hold = 0;
            g_sine_index++;
            if (g_sine_index >= SINE_SAMPLES)
            {
                g_sine_index = 0;
            }
            g_pwm_duty = g_sine_duty_table[g_sine_index];
        }
    }
}

void main(void)
{
    Clock_Init24M();
    Port_Init();

    g_pwm_counter = 0;
    g_sine_index = 0;
    g_sine_hold = 0;
    g_pwm_duty = g_sine_duty_table[0];

    Timer2_Init();
    EA = 1;

    while (1)
    {
        ;
    }
}
