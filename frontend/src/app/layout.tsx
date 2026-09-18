import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Genetic Cars · Race Lab",
  description: "Train steering networks. Watch evolution find the road.",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
