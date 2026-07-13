import type { SVGProps } from "react";

export function NectarMark(props: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 48 48" aria-hidden="true" {...props}>
      <defs>
        <linearGradient
          id="nectar-gradient"
          x1="8"
          y1="6"
          x2="40"
          y2="42"
          gradientUnits="userSpaceOnUse"
        >
          <stop stopColor="#ffd86a" />
          <stop offset="1" stopColor="#e89918" />
        </linearGradient>
      </defs>
      <path
        fill="url(#nectar-gradient)"
        d="M22.2 3.7a3.6 3.6 0 0 1 3.6 0l13.8 8a3.6 3.6 0 0 1 1.8 3.1v16a3.6 3.6 0 0 1-1.8 3.1l-13.8 8a3.6 3.6 0 0 1-3.6 0L8.4 34a3.6 3.6 0 0 1-1.8-3.1v-16a3.6 3.6 0 0 1 1.8-3.1l13.8-8Z"
      />
      <path
        fill="#4b3411"
        d="M17 14.5h6.4L31 24v-9.5h5V33h-5.6L22 22.6V33h-5V14.5Z"
      />
      <circle cx="14.2" cy="14.3" r="2.1" fill="#fff5cd" />
    </svg>
  );
}
