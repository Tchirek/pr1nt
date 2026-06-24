import type { Config } from "tailwindcss";

const config: Config = {
  content: ["./app/**/*.{ts,tsx}", "./components/**/*.{ts,tsx}", "./server/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        canvas: "#eef6ef",
        ink: "#122018",
        line: "#cdddcf",
        panel: "#ffffff",
        success: "#166534",
        warning: "#8a6c10",
        danger: "#b91c1c",
      },
      boxShadow: {
        card: "0 24px 60px rgba(18, 32, 24, 0.08)",
      },
      fontFamily: {
        sans: ["'IBM Plex Sans'", "ui-sans-serif", "system-ui", "sans-serif"],
      },
    },
  },
  plugins: [],
};

export default config;
