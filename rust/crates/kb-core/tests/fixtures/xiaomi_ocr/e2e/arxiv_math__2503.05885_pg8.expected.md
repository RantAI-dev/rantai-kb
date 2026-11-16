## 2.2 Upper bounds on the mass distribution

Our main results give lower bounds on the mass measure  $ m^{\nu} $ defined in (1.14). It is then natural to ask about matching upper bounds. Additionally, we only give lower bounds for  $ |k| \ll \nu^{-1/2} $. For  $ |k| \gg \nu^{-1/2} $, the dynamics should be dominated by the  $ \nu\Delta $ dissipation, and so the lower bound should not generally hold.

2.2.1. The  $ |k| \ll \nu^{-1/2} $ range. First, when  $ |k| \ll \nu^{-1/2} $, we note the following upper bound. This bound is essentially identical to the cumulative Batchelor spectrum upper bound obtained in [BBPS21b] and relies on the uniform-in-diffusivity exponential mixing.

**Proposition 2.1.** For all  $ R \geq 2 $,

 $$ m^{\nu}([1, R]) \leqslant C \log R. $$ 

(2.2)

**Proof.** Using the exponential mixing, we note that for any  $ \alpha > 0 $

 $$ m^{\nu}([1, R]) = \int_{0}^{\infty} \mathbb{E} \| \Pi_{\leqslant R} \varphi_t^{\nu} \|_{L^2}^2 \\
\leqslant \int_{0}^{\alpha \log R} \mathbb{E} \| \varphi_t^{\nu} \|_{L^2}^2 + CR^2 \int_{\alpha \log R}^{\infty} e^{-C^{-1}t} dt \\
\leqslant \alpha \log R + CR^2 e^{-C^{-1} \alpha \log R}. $$ 

Choosing  $ \alpha $ large enough, we conclude.

One can then use Chebyshev's inequality to obtain a log-density bound on the “overcharged” radii. We define

 $$ \overline{B}_{h, \alpha}^{\nu} := \{ r \in [1, \infty) : m^{\nu}([r, r+h]) \geqslant \frac{\alpha h}{r} \} $$ 

(2.3)

and compute

 $$ \mu_{1,R}(\overline{B}_{h, \alpha}^{\nu}) = \frac{1}{\log R} \int_{\overline{B}_{h, \alpha}^{\nu} \cap [1, R]} \frac{1}{r} dr \leqslant \frac{1}{\alpha h \log R} \int_{1}^{R} m^{\nu}([r, r+h]) dr \leqslant \frac{1}{\alpha \log R} m^{\nu}([1, R+h]) \leqslant C \alpha^{-1}. $$ 

We have proven the following density estimate.

**Proposition 2.2.** For all  $ R \geq 2 $,

 $$ \mu_{1,R}(\overline{B}_{h, \alpha}^{\nu}) \leqslant C \alpha^{-1}. $$ 

We note that, in some sense, this upper bound matches the second term of the lower bound density estimate in (1.10) or the lower bound estimate in (1.12). It however does not match the estimates (1.11) or (1.13), which do not require taking a limit in  $ \alpha $.

2.2.2. The  $ |k| \gg \nu^{-1/2} $ range. The bound given by Proposition 2.1 holds for all  $ R \geq 2 $, however one can get a much sharper statement for  $ R \gg \nu^{-1/2} $. Computing the Itô differential, we have the energy estimate

 $$ \frac{d}{dt} \mathbb{E} \| \psi_t^{\nu} \|_{L^2}^2 = -2\nu \mathbb{E} \| \nabla \psi_t^{\nu} \|_{L^2}^2 + \| g \|_{L^2}^2. $$ 

By the Itô isometry, Duhamel's principle, and the stationarity of the  $ u_t $ process,

 $$ \frac{d}{dt} \mathbb{E} \| \psi_t^{\nu} \|_{L^2}^2 = \mathbb{E} \| \varphi_t^{\nu} \|_{L^2}^2 \leqslant e^{-C^{-1} \nu t} \xrightarrow{t \to \infty} 0. $$ 