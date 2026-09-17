/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        forge: {
          bg: "#1e222c",
          panel: "#262b38",
          edge: "#333a4b",
          text: "#ebeef5",
          dim: "#9aa3b5",
          accent: "#ffb020",
        },
      },
    },
  },
  plugins: [],
};
