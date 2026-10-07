import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Trailway",
  description: "Deploy apps to your own servers.",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
