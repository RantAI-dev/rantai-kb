FIGURE 3. The decorated fan of an action of  $ Q(1)^2 $ on  $ \mathbb{P}^{2|2} $

**Example 4.28.** Let us use Proposition 4.26 and Corollary 4.27 to classify toric supervarieties with supertorus  $ Q(1)^n $ and underlying variety  $ \mathbb{P}^n \cong X_\Sigma $ for the complete fan whose rays are  $ \rho_i = \mathbb{R}_+ x_i $ for  $ i = 1, ..., n $ and  $ \rho_0 = \mathbb{R}_+ (-x_1 - ... - x_n) $.

For  $ i = 1, ..., n $, the ray  $ \rho_i $ must be decorated by a subspace  $ V_{\rho_i} $ such that  $ [V_{\rho_i}, V_{\rho_i}] \subseteq \mathbb{C}x_i $. Hence  $ V_{\rho_i} = \mathbb{C}\theta_i $ or 0. Likewise, unless  $ V_{\rho_0} = 0 $, we have  $ V_{\rho_0} = \mathbb{C}(\theta_1 \pm \theta_2 \pm ... \pm \theta_n) $ for some  $ 2^{n-1} $ choices of  $ \pm $.

It is straightforward to verify that condition (b) of the definition of a large-orbit decorated fan holds regardless of which subspaces are chosen. We therefore obtain a collection of  $ 2^n(1 + 2^{n-1}) $ toric supervarieties which are not equivariantly isomorphic. Many, however, are isomorphic via toric morphisms (to be defined in the following section).

Notice that if all but one of these decorations is nonzero, then the decorated fan describes a supervariety isomorphic to projective superspace  $ \mathbb{P}^{n|n} $. For instance, if  $ V_{\rho_i} = \mathbb{C}\theta_i $ and  $ V_{\rho_0} = 0 $, then the coordinate superalgebras of the affine charts can be written as

 $$ \mathbb{C}[t_1, ..., t_n, t_1\xi_1, ..., t_n\xi_n] $$ 

and

 $$ \mathbb{C}[t_i^{-1}t_1, ..., t_i^{-1}, ..., t_i^{-1}t_n\xi_1, ..., \xi_i, ..., t_i^{-1}t_n\xi_n]. $$ 

Figure 3 depicts the corresponding decorated fan for  $ n = 2 $,

If instead we change the decoration of  $ \rho_0 $ to  $ \theta_1 + ... + \theta_n $, then the resulting affine charts have coordinate superalgebras

 $$ \mathbb{C}[t_1, ..., t_n, t_1\xi_1, ..., t_n\xi_n] $$ 

and

 $$ \mathbb{C}[t_i^{-1}t_1(1+\xi_i\xi_1),...,t_i^{-1},...,t_i^{-1}t_n(1+\xi_i\xi_n),t_i^{-1}t_1(\xi_i-\xi_1),...,t_i^{-1}\xi_i,...,t_i^{-1}t_n(\xi_i-\xi_n)], $$ 

so the supervariety is decidedly not isomorphic to projective superspace. Figure 4(B) depicts corresponding decorated fan for  $ n = 1 $.

In general, when  $ T = Q(1)^n $, there are finitely many possible decorations for each ray. Namely, for  $ \rho = \mathbb{R}_+(a_1x_1 + \ldots + a_nx_n) $, there are  $ 2^{d-1} $ many possible “square root subspaces”  $ \mathbb{C}(\sqrt{a_1}\theta_1 \pm \ldots \pm \sqrt{a_n}\theta_n) $, where  $ d $ is the number of indices  $ i = 1, \ldots, n $ for which  $ a_i \neq 0 $.

The prior example admitted no issues of compatibility between different rays of the same cone. This is not ordinarily the case; if  $ \rho_1 = \mathbb{R}_+(a_1x_1 + \ldots + a_nx_n) $ and  $ \rho_2 = \mathbb{R}_+(b_1x_1 + \ldots + b_nx_n) $, compatibility is less common if the linear matroid on the  $ n $ vectors