;;;; Errors
(define-constant ERR_PANIC 0)
(define-constant ERR_NAMESPACE_PREORDER_NOT_FOUND 1001)
(define-constant ERR_NAMESPACE_PREORDER_EXPIRED 1002)
(define-constant ERR_NAMESPACE_PREORDER_ALREADY_EXISTS 1003)
(define-constant ERR_NAMESPACE_UNAVAILABLE 1004)
(define-constant ERR_NAMESPACE_NOT_FOUND 1005)
(define-constant ERR_NAMESPACE_ALREADY_EXISTS 1006)
(define-constant ERR_NAMESPACE_NOT_LAUNCHED 1007)
(define-constant ERR_NAMESPACE_PRICE_FUNCTION_INVALID 1008)
(define-constant ERR_NAMESPACE_PREORDER_CLAIMABILITY_EXPIRED 1009)
(define-constant ERR_NAMESPACE_PREORDER_LAUNCHABILITY_EXPIRED 1010)
(define-constant ERR_NAMESPACE_OPERATION_UNAUTHORIZED 1011)
(define-constant ERR_NAMESPACE_STX_BURNT_INSUFFICIENT 1012)
(define-constant ERR_NAMESPACE_BLANK 1013)
(define-constant ERR_NAMESPACE_ALREADY_LAUNCHED 1014)
(define-constant ERR_NAMESPACE_HASH_MALFORMED 1015)
(define-constant ERR_NAMESPACE_CHARSET_INVALID 1016)

(define-constant ERR_NAME_PREORDER_NOT_FOUND 2001)
(define-constant ERR_NAME_PREORDER_EXPIRED 2002)
(define-constant ERR_NAME_PREORDER_FUNDS_INSUFFICIENT 2003)
(define-constant ERR_NAME_UNAVAILABLE 2004)
(define-constant ERR_NAME_OPERATION_UNAUTHORIZED 2006)
(define-constant ERR_NAME_STX_BURNT_INSUFFICIENT 2007)
(define-constant ERR_NAME_EXPIRED 2008)
(define-constant ERR_NAME_GRACE_PERIOD 2009)
(define-constant ERR_NAME_BLANK 2010)
(define-constant ERR_NAME_ALREADY_CLAIMED 2011)
(define-constant ERR_NAME_CLAIMABILITY_EXPIRED 2012)
(define-constant ERR_NAME_NOT_FOUND 2013)
(define-constant ERR_NAME_REVOKED 2014)
(define-constant ERR_NAME_TRANSFER_FAILED 2015)
(define-constant ERR_NAME_PREORDER_ALREADY_EXISTS 2016)
(define-constant ERR_NAME_HASH_MALFORMED 2017)
(define-constant ERR_NAME_PREORDERED_BEFORE_NAMESPACE_LAUNCH 2018)
(define-constant ERR_NAME_NOT_RESOLVABLE 2019)
(define-constant ERR_NAME_COULD_NOT_BE_MINTED 2020)
(define-constant ERR_NAME_COULD_NOT_BE_TRANSFERED 2021)
(define-constant ERR_NAME_CHARSET_INVALID 2022)

(define-constant ERR_PRINCIPAL_ALREADY_ASSOCIATED 3001)
(define-constant ERR_INSUFFICIENT_FUNDS 4001)

(define-constant NAMESPACE_PREORDER_CLAIMABILITY_TTL u10)
(define-constant NAMESPACE_LAUNCHABILITY_TTL u10)
(define-constant NAME_PREORDER_CLAIMABILITY_TTL u10)
(define-constant NAME_GRACE_PERIOD_DURATION u5)

(define-constant ATTACHMENTS_INV_PAGE_SIZE u8)
(define-data-var attachments-inv-index-cursor uint u0)
(define-data-var attachments-inv-page-cursor uint u0)
(define-map attachments-inv 
    ((page uint) (index uint)) 
    ((content-hash (buff 20))))

;; Price tables
(define-constant NAMESPACE_PRICE_TIERS (list
  u96000 
  u9600 u9600 
  u960 u960 u960 u960 
  u96 u96 u96 u96 u96 u96 u96 u96 u96 u96 u96 u96 u96))

;;;; Data
(define-map namespaces
  ((namespace (buff 20)))
  ((namespace-import principal)
   (revealed-at uint)
   (launched-at (optional uint))
   (lifetime uint)
   (price-function (tuple 
    (buckets (list 16 uint)) 
    (base uint) 
    (coeff uint) 
    (nonalpha-discount uint) 
    (no-vowel-discount uint)))))

(define-map namespace-preorders
  ((hashed-salted-namespace (buff 20)) (buyer principal))
  ((created-at uint) (claimed bool) (stx-burned uint)))

(define-non-fungible-token names (tuple (name (buff 32)) (namespace (buff 20))))

;; Rule 1-1 -> 1 principal, 1 name
(define-map owner-name ((owner principal)) ((name (buff 32)) (namespace (buff 20))))

(define-map name-properties
  ((name (buff 32)) (namespace (buff 20)))
  ((registered-at (optional uint))
   (imported-at (optional uint))
   (revoked-at (optional uint))
   (zonefile-hash (buff 20))))

(define-map name-preorders
  ((hashed-salted-fqn (buff 20)) (buyer principal))
  ((created-at uint) (claimed bool) (stx-burned uint)))

(define-private (min (a uint) (b uint))
  (if (<= a b) a b))

(define-private (max (a uint) (b uint))
  (if (> a b) a b))

(define-read-only (compute-namespace-price? (namespace (buff 20)))
  (let ((namespace-len (len namespace)))
    (asserts!
      (> namespace-len u0)
      (err ERR_NAMESPACE_BLANK))
    (ok (get value (fold 
      element-at 
      NAMESPACE_PRICE_TIERS 
      (tuple (limit (min u8 namespace-len)) (cursor u0) (value u0)))))))

(define-private (element-at (i uint) (acc (tuple (limit uint) (cursor uint) (value uint))))
  (if (is-eq (get cursor acc) (get limit acc))
    (tuple (limit (get limit acc)) (cursor (+ u1 (get cursor acc))) (value i))
    (tuple (limit (get limit acc)) (cursor (+ u1 (get cursor acc))) (value (get value acc)))))
  
(define-private (get-exp-at-index (buckets (list 16 uint)) (index uint))
  (get value (fold element-at buckets (tuple (limit index) (cursor u0) (value u0)))))

(define-private (is-digit (char (buff 1)))
  (or 
    (is-eq char 0x30) ;; 0
    (is-eq char 0x31) ;; 1
    (is-eq char 0x32) ;; 2
    (is-eq char 0x33) ;; 3
    (is-eq char 0x34) ;; 4
    (is-eq char 0x35) ;; 5
    (is-eq char 0x36) ;; 6
    (is-eq char 0x37) ;; 7
    (is-eq char 0x38) ;; 8
    (is-eq char 0x39))) ;; 9

(define-private (is-lowercase-alpha (char (buff 1)))
  (or 
    (is-eq char 0x61) ;; a
    (is-eq char 0x62) ;; b
    (is-eq char 0x63) ;; c
    (is-eq char 0x64) ;; d
    (is-eq char 0x65) ;; e
    (is-eq char 0x66) ;; f
    (is-eq char 0x67) ;; g
    (is-eq char 0x68) ;; h
    (is-eq char 0x69) ;; i
    (is-eq char 0x6a) ;; j
    (is-eq char 0x6b) ;; k
    (is-eq char 0x6c) ;; l
    (is-eq char 0x6d) ;; m
    (is-eq char 0x6e) ;; n
    (is-eq char 0x6f) ;; o
    (is-eq char 0x70) ;; p
    (is-eq char 0x71) ;; q
    (is-eq char 0x72) ;; r
    (is-eq char 0x73) ;; s
    (is-eq char 0x74) ;; t
    (is-eq char 0x75) ;; u
    (is-eq char 0x76) ;; v
    (is-eq char 0x77) ;; w
    (is-eq char 0x78) ;; x
    (is-eq char 0x79) ;; y
    (is-eq char 0x7a))) ;; z

(define-private (is-vowel (char (buff 1)))
  (or 
    (is-eq char 0x61) ;; a
    (is-eq char 0x65) ;; e
    (is-eq char 0x69) ;; i
    (is-eq char 0x6f) ;; o
    (is-eq char 0x75) ;; u
    (is-eq char 0x79))) ;; y

(define-private (is-special-char (char (buff 1)))
  (or 
    (is-eq char 0x2d) ;; -
    (is-eq char 0x5f))) ;; _

(define-private (is-char-valid (char (buff 1)))
  (or 
    (is-lowercase-alpha char)
    (is-digit char)
    (is-special-char char)))

(define-private (is-nonalpha (char (buff 1)))
  (or 
    (is-digit char)
    (is-special-char char)))

(define-private (has-vowels-chars (name (buff 32)))
  (> (len (filter is-vowel name)) u0))

(define-private (has-nonalpha-chars (name (buff 32)))
  (> (len (filter is-nonalpha name)) u0))

(define-private (has-invalid-chars (name (buff 32)))
  (< (len (filter is-char-valid name)) (len name)))

(define-private (name-lease-started-at? (namespace-launched-at (optional uint)) 
                                        (namespace-revealed-at uint)
                                        (name-props (tuple 
                                                  (registered-at (optional uint))
                                                  (imported-at (optional uint))
                                                  (revoked-at (optional uint))
                                                  (zonefile-hash (buff 20)))))
      (let ((registered-at (get registered-at name-props))
            (imported-at (get imported-at name-props)))
        ;; The namespace must be launched
        (asserts! (is-some namespace-launched-at) (err ERR_NAMESPACE_NOT_LAUNCHED))
        ;; Sanity check: the name must have been either be registered or imported
        (asserts! (is-eq (xor 
          (match (get registered-at name-props) res 1 0)
          (match (get imported-at name-props)   res 1 0)) 1) (err ERR_PANIC))
        ;; If the name was launched, then started-at will come from registered-at
        (if (is-some registered-at)
          ;; The name was registered - We return the registration block height
          (ok (unwrap-panic registered-at))
          (if (and (>= (unwrap-panic imported-at) namespace-revealed-at)
                   (<= (unwrap-panic imported-at) (unwrap-panic namespace-launched-at)))
            ;; The name was imported after revealing the namespace and before launching the namespace - We return the launch block height
            (ok (unwrap-panic namespace-launched-at))
            (err ERR_NAME_EXPIRED)))))

(define-private (compute-name-price (name (buff 32))
                                    (price-function (tuple (buckets (list 16 uint)) (base uint) (coeff uint) (nonalpha-discount uint) (no-vowel-discount uint))))
  (let (
    (exponent (get-exp-at-index (get buckets price-function) (min u15 (- (len name) u1))))
    (no-vowel-discount (if (not (has-vowels-chars name)) (get no-vowel-discount price-function) u1))
    (nonalpha-discount (if (has-nonalpha-chars name) (get nonalpha-discount price-function) u1)))
    (*
      (/
        (*
          (get coeff price-function)
          (pow (get base price-function) exponent))
        (max nonalpha-discount no-vowel-discount))
      u10)))

(define-private (is-name-lease-expired? (namespace (buff 20)) (name (buff 32)))
  (let (
    (namespace-props (unwrap! 
      (map-get? namespaces ((namespace namespace))) 
      (err ERR_NAMESPACE_NOT_FOUND)))
    (name-props (unwrap! 
      (map-get? name-properties ((namespace namespace) (name name))) 
      (err ERR_NAME_NOT_FOUND))))
    (let ((lease-started-at (try! (name-lease-started-at? (get launched-at namespace-props) (get revealed-at namespace-props) name-props)))
          (lifetime (get lifetime namespace-props)))
        (if (is-eq lifetime u0)
          (ok false)
          (ok (> block-height (+ lifetime lease-started-at)))))))

(define-read-only (is-name-in-grace-period? (namespace (buff 20)) (name (buff 32)))
  (let (
    (namespace-props (unwrap! 
      (map-get? namespaces ((namespace namespace))) 
      (err ERR_NAMESPACE_NOT_FOUND)))
    (name-props (unwrap! 
      (map-get? name-properties ((namespace namespace) (name name))) 
      (err ERR_NAME_NOT_FOUND))))
    (let ((lease-started-at (try! (name-lease-started-at? (get launched-at namespace-props) (get revealed-at namespace-props) name-props)))
          (lifetime (get lifetime namespace-props)))
        (if (is-eq lifetime u0)
          (ok false)
          (ok (and 
            (> block-height (+ lifetime lease-started-at)) 
            (<= block-height (+ (+ lifetime lease-started-at) NAME_GRACE_PERIOD_DURATION))))))))

(define-private (update-name-ownership? (namespace (buff 20)) 
                                        (name (buff 32)) 
                                        (from principal) 
                                        (to principal))
  (if (is-eq from to)
    (ok true)
    (begin
      (unwrap!
        (nft-transfer? names (tuple (name name) (namespace namespace)) from to)
        (err ERR_NAME_COULD_NOT_BE_TRANSFERED))
      (map-delete owner-name ((owner from)))
      (map-set owner-name
        ((owner to))
        ((namespace namespace) (name name)))
      (ok true))))

(define-private (update-zonefile-and-props (namespace (buff 20))
                                           (name (buff 32))
                                           (registered-at (optional uint)) 
                                           (imported-at (optional uint)) 
                                           (revoked-at (optional uint)) 
                                           (zonefile-hash (buff 20)))
  (let 
    ((current-page (var-get attachments-inv-page-cursor))
    (current-index (var-get attachments-inv-index-cursor)))
    (let 
      ((next-page (if (is-eq (+ current-index u1) ATTACHMENTS_INV_PAGE_SIZE)
        (+ current-page u1)
        current-page))
      (next-index (mod (+ current-index u1) ATTACHMENTS_INV_PAGE_SIZE)))
      ;; Emit event used as a system hinter
      (print {
        attachment: {
          hash: zonefile-hash,
          page-index: current-page,
          position-in-page: current-index,
          metadata: {
            name: name,
            namespace: namespace,
            tx-sender: tx-sender
          }
        }})
      ;; Update attachments-inv
      (map-set attachments-inv
        ((page current-page) (index current-index))
        ((content-hash zonefile-hash)))
      ;; Update cursors
      (var-set attachments-inv-page-cursor next-page)
      (var-set attachments-inv-index-cursor next-index)
      (map-set name-properties
        ((namespace namespace) (name name))
        ((registered-at registered-at)
          (imported-at imported-at)
          (revoked-at revoked-at)
          (zonefile-hash zonefile-hash))))))

(define-read-only (get-attachments-inv-info)
  (ok { 
    pages-count: (+ (var-get attachments-inv-page-cursor) u1),
    last-page-len: (var-get attachments-inv-index-cursor),
    page-size: ATTACHMENTS_INV_PAGE_SIZE
  }))

;;;; NAMESPACES
;; NAMESPACE_PREORDER
;; This step registers the salted hash of the namespace with BNS nodes, and burns the requisite amount of cryptocurrency.
;; Additionally, this step proves to the BNS nodes that user has honored the BNS consensus rules by including a recent
;; consensus hash in the transaction.
;; Returns pre-order's expiration date (in blocks).
(define-public (namespace-preorder (hashed-salted-namespace (buff 20))
                                   (stx-to-burn uint))
  (let 
    ((former-preorder 
      (map-get? namespace-preorders ((hashed-salted-namespace hashed-salted-namespace) (buyer tx-sender)))))
    ;; Ensure eventual former pre-order expired 
    (asserts! 
      (if (is-none former-preorder)
        true
        (>= block-height (+ NAMESPACE_PREORDER_CLAIMABILITY_TTL
                            (unwrap! (get created-at former-preorder) (err ERR_PANIC)))))
      (err ERR_NAMESPACE_PREORDER_ALREADY_EXISTS))
    ;; Ensure that the hashed namespace is 20 bytes long
    (asserts! (is-eq (len hashed-salted-namespace) u20) (err ERR_NAMESPACE_HASH_MALFORMED))
    ;; Ensure that user will be burning a positive amount of tokens
    (asserts! (> stx-to-burn u0) (err ERR_NAMESPACE_STX_BURNT_INSUFFICIENT))
    ;; Burn the tokens
    (unwrap! (stx-burn? stx-to-burn tx-sender) (err ERR_INSUFFICIENT_FUNDS))
    ;; Register the preorder
    (map-set namespace-preorders
      ((hashed-salted-namespace hashed-salted-namespace) (buyer tx-sender))
      ((created-at block-height) (claimed false) (stx-burned stx-to-burn)))
    (ok (+ block-height NAMESPACE_PREORDER_CLAIMABILITY_TTL))))

;; NAMESPACE_REVEAL
;; This second step reveals the salt and the namespace ID (pairing it with its NAMESPACE_PREORDER). It reveals how long
;; names last in this namespace before they expire or must be renewed, and it sets a price function for the namespace
;; that determines how cheap or expensive names its will be.
(define-public (namespace-reveal (namespace (buff 20))
                                 (namespace-salt (buff 20))
                                 (p-func-base uint)
                                 (p-func-coeff uint)
                                 (p-func-b1 uint)
                                 (p-func-b2 uint)
                                 (p-func-b3 uint)
                                 (p-func-b4 uint)
                                 (p-func-b5 uint)
                                 (p-func-b6 uint)
                                 (p-func-b7 uint)
                                 (p-func-b8 uint)
                                 (p-func-b9 uint)
                                 (p-func-b10 uint)
                                 (p-func-b11 uint)
                                 (p-func-b12 uint)
                                 (p-func-b13 uint)
                                 (p-func-b14 uint)
                                 (p-func-b15 uint)
                                 (p-func-b16 uint)
                                 (p-func-non-alpha-discount uint)
                                 (p-func-no-vowel-discount uint)
                                 (lifetime uint)
                                 (namespace-import principal))
  ;; The salt and namespace must hash to a preorder entry in the `namespace_preorders` table.
  ;; The sender must match the principal in the preorder entry (implied)
  (let (
    (hashed-salted-namespace (hash160 (concat namespace namespace-salt)))
    (price-function (tuple 
      (buckets (list
        p-func-b1
        p-func-b2
        p-func-b3
        p-func-b4
        p-func-b5
        p-func-b6
        p-func-b7
        p-func-b8
        p-func-b9
        p-func-b10
        p-func-b11
        p-func-b12
        p-func-b13
        p-func-b14
        p-func-b15
        p-func-b16))
      (base p-func-base)
      (coeff p-func-coeff)
      (nonalpha-discount p-func-non-alpha-discount)
      (no-vowel-discount p-func-no-vowel-discount))))
    (let (
      (preorder (unwrap!
        (map-get? namespace-preorders ((hashed-salted-namespace hashed-salted-namespace) (buyer tx-sender)))
        (err ERR_NAMESPACE_PREORDER_NOT_FOUND)))
      (namespace-price (unwrap! 
        (compute-namespace-price? namespace)
        (err ERR_NAMESPACE_BLANK))))
    ;; The namespace must only have valid chars
    (asserts!
      (not (has-invalid-chars namespace))
      (err ERR_NAMESPACE_CHARSET_INVALID))
    ;; The namespace must not exist yet in the `namespaces` table
    (asserts!
      (is-none (map-get? namespaces ((namespace namespace))))
      (err ERR_NAMESPACE_ALREADY_EXISTS))
    ;; The amount burnt must be equal to or greater than the cost of the namespace
    (asserts!
      (>= (get stx-burned preorder) namespace-price)
      (err ERR_NAMESPACE_STX_BURNT_INSUFFICIENT))
    ;; This transaction must arrive within 24 hours of its `NAMESPACE_PREORDER`
    (asserts!
      (< block-height (+ (get created-at preorder) NAMESPACE_PREORDER_CLAIMABILITY_TTL))
      (err ERR_NAMESPACE_PREORDER_CLAIMABILITY_EXPIRED))
    ;; The preorder record for this namespace will be marked as "claimed"
    (map-set namespace-preorders
      ((hashed-salted-namespace hashed-salted-namespace) (buyer tx-sender))
      ((created-at (get created-at preorder)) (claimed true) (stx-burned (get stx-burned preorder))))
    ;; The namespace will be set as "revealed" but not "launched", its price function, its renewal rules, its version,
    ;; and its import principal will be written to the  `namespaces` table.
    (map-set namespaces
      ((namespace namespace))
      ((namespace-import namespace-import)
       (revealed-at block-height)
       (launched-at none)
       (lifetime lifetime)
       (price-function price-function)))
    (ok true))))

;; NAME_IMPORT
;; Once a namespace is revealed, the user has the option to populate it with a set of names. Each imported name is given
;; both an owner and some off-chain state. This step is optional; Namespace creators are not required to import names.
(define-public (name-import (namespace (buff 20))
                            (name (buff 32))
                            (zonefile-hash (buff 20)))
  (let (
    (namespace-props (unwrap!
      (map-get? namespaces ((namespace namespace)))
      (err ERR_NAMESPACE_NOT_FOUND))))
      ;; The sender principal must match the namespace's import principal
      (asserts!
        (is-eq (get namespace-import namespace-props) tx-sender)
        (err ERR_NAMESPACE_OPERATION_UNAUTHORIZED))
      ;; The name's namespace must not be launched
      (asserts!
        (is-none (get launched-at namespace-props))
        (err ERR_NAMESPACE_ALREADY_LAUNCHED))
      ;; Less than 1 year must have passed since the namespace was "revealed"
      (asserts!
        (< block-height (+ (get revealed-at namespace-props) NAMESPACE_LAUNCHABILITY_TTL))
        (err ERR_NAMESPACE_PREORDER_LAUNCHABILITY_EXPIRED))
      ;; Mint the new name
      (if (is-ok (nft-mint? names (tuple (namespace namespace) (name name)) tx-sender))
        true
        false)
      ;; Update zonefile and props
      (update-zonefile-and-props
        namespace 
        name  
        none
        (some block-height) ;; Set imported-at
        none
        zonefile-hash)
      (ok true)))

;; NAMESPACE_READY
;; The final step of the process launches the namespace and makes the namespace available to the public. Once a namespace
;; is launched, anyone can register a name in it if they pay the appropriate amount of cryptocurrency.
(define-public (namespace-ready (namespace (buff 20)))
  (let (
      (namespace-props (unwrap!
        (map-get? namespaces ((namespace namespace)))
        (err ERR_NAMESPACE_NOT_FOUND))))
    ;; The sender principal must match the namespace's import principal
    (asserts!
      (is-eq (get namespace-import namespace-props) tx-sender)
      (err ERR_NAMESPACE_OPERATION_UNAUTHORIZED))
    ;; The name's namespace must not be launched
    (asserts!
      (is-none (get launched-at namespace-props))
      (err ERR_NAMESPACE_ALREADY_LAUNCHED))
    ;; Less than 1 year must have passed since the namespace was "revealed"
    (asserts!
      (< block-height (+ (get revealed-at namespace-props) NAMESPACE_LAUNCHABILITY_TTL))
      (err ERR_NAMESPACE_PREORDER_LAUNCHABILITY_EXPIRED))
    ;; The namespace will be set to "launched"
    (map-set namespaces
      ((namespace namespace))
      ((launched-at (some block-height))
       (namespace-import (get namespace-import namespace-props))
       (revealed-at (get revealed-at namespace-props))
       (lifetime (get lifetime namespace-props))
       (price-function (get price-function namespace-props))))
    (ok true)))

;;;; NAMES

;; NAME_PREORDER
;; This is the first transaction to be sent. It tells all BNS nodes the salted hash of the BNS name,
;; and it pays the registration fee to the namespace owner's designated address
(define-public (name-preorder (hashed-salted-fqn (buff 20))
                              (stx-to-burn uint))
  (let 
    ((former-preorder 
      (map-get? name-preorders ((hashed-salted-fqn hashed-salted-fqn) (buyer tx-sender)))))
    ;; Ensure eventual former pre-order expired 
    (asserts! 
      (if (is-none former-preorder)
        true
        (>= block-height (+ NAME_PREORDER_CLAIMABILITY_TTL
                            (unwrap! (get created-at former-preorder) (err ERR_PANIC)))))
      (err ERR_NAME_PREORDER_ALREADY_EXISTS))
          (asserts! (> stx-to-burn u0) (err ERR_NAMESPACE_STX_BURNT_INSUFFICIENT))    
    ;; Ensure that the hashed fqn is 20 bytes long
    (asserts! (is-eq (len hashed-salted-fqn) u20) (err ERR_NAME_HASH_MALFORMED))
    ;; Ensure that user will be burning a positive amount of tokens
    (asserts! (> stx-to-burn u0) (err ERR_NAME_STX_BURNT_INSUFFICIENT))
    ;; Burn the tokens
    (unwrap! (stx-burn? stx-to-burn tx-sender) (err ERR_INSUFFICIENT_FUNDS))
    ;; Register the pre-order
    (map-set name-preorders
      ((hashed-salted-fqn hashed-salted-fqn) (buyer tx-sender))
      ((created-at block-height) (stx-burned stx-to-burn) (claimed false)))
    (ok (+ block-height NAME_PREORDER_CLAIMABILITY_TTL))))

;; NAME_REGISTRATION
;; This is the second transaction to be sent. It reveals the salt and the name to all BNS nodes,
;; and assigns the name an initial public key hash and zone file hash
(define-public (name-register (namespace (buff 20))
                              (name (buff 32))
                              (salt (buff 20))
                              (zonefile-hash (buff 20)))
  (let (
    (can-sender-register-name (try! (can-register-name tx-sender)))
    (hashed-salted-fqn (hash160 (concat (concat (concat name 0x2e) namespace) salt))))
    (let ( 
        (preorder (unwrap!
          (map-get? name-preorders ((hashed-salted-fqn hashed-salted-fqn) (buyer tx-sender)))
          (err ERR_NAME_PREORDER_NOT_FOUND)))
        (namespace-props (unwrap!
          (map-get? namespaces ((namespace namespace)))
          (err ERR_NAMESPACE_NOT_FOUND)))
        (current-owner (nft-get-owner? names (tuple (name name) (namespace namespace)))))
      ;; The name must only have valid chars
      (asserts!
        (not (has-invalid-chars name))
        (err ERR_NAME_CHARSET_INVALID))
      ;; The name must not exist yet, or be expired
      (if (is-none current-owner)
        true
        (asserts!
          (unwrap! (is-name-lease-expired? namespace name) (err ERR_PANIC))
          (err ERR_NAME_UNAVAILABLE)))
      ;; The name's namespace must be launched
      (asserts!
        (is-some (get launched-at namespace-props))
        (err ERR_NAMESPACE_NOT_LAUNCHED))
      ;; The preorder must have been created after the launch of the namespace
      (asserts!
        (> (get created-at preorder) (unwrap! (get launched-at namespace-props) (err ERR_PANIC)))
        (err ERR_NAME_PREORDERED_BEFORE_NAMESPACE_LAUNCH))
      ;; The preorder entry must be unclaimed
      (asserts!
        (is-eq (get claimed preorder) false)
        (err ERR_NAME_ALREADY_CLAIMED))
      ;; Less than 24 hours must have passed since the name was preordered
      (asserts!
        (< block-height (+ (get created-at preorder) NAME_PREORDER_CLAIMABILITY_TTL))
        (err ERR_NAME_CLAIMABILITY_EXPIRED))
      ;; The amount burnt must be equal to or greater than the cost of the name
      (asserts!
        (>= (get stx-burned preorder) (compute-name-price name (get price-function namespace-props)))
        (err ERR_NAME_STX_BURNT_INSUFFICIENT))
      ;; The principal can register a name
      (asserts!
        can-sender-register-name
        (err ERR_PRINCIPAL_ALREADY_ASSOCIATED))
      ;; Mint the name if new, transfer the name otherwise.
      (if (is-none current-owner)
        (begin
          (unwrap! 
            (nft-mint? 
              names 
              (tuple (namespace namespace) (name name)) 
              tx-sender)
            (err ERR_NAME_COULD_NOT_BE_MINTED))
          (map-set owner-name
            ((owner tx-sender))
            ((namespace namespace) (name name))))
        (if (is-eq tx-sender (unwrap! current-owner (err ERR_PANIC)))
          true
          (let ((previous-owner (unwrap! current-owner (err ERR_PANIC)))) 
            (unwrap!
              (update-name-ownership? namespace name previous-owner tx-sender)
              (err ERR_NAME_COULD_NOT_BE_TRANSFERED)))))
      ;; Update name's metadata / properties
      (update-zonefile-and-props
        namespace 
        name  
        (some block-height)
        none
        none
        zonefile-hash)
      (ok true))))

;; NAME_UPDATE
;; A NAME_UPDATE transaction changes the name's zone file hash. You would send one of these transactions 
;; if you wanted to change the name's zone file contents. 
;; For example, you would do this if you want to deploy your own Gaia hub and want other people to read from it.
(define-public (name-update (namespace (buff 20))
                            (name (buff 32))
                            (zonefile-hash (buff 20)))
  (let (
    (namespace-props (unwrap!
      (map-get? namespaces ((namespace namespace)))
      (err ERR_NAMESPACE_NOT_FOUND))) ;; The namespace must exist
    (owner (unwrap!
      (nft-get-owner? names (tuple (name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND))) ;; The name must exist
    (name-props (unwrap!
      (map-get? name-properties ((name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND)))) ;; The name must exist
    ;; The name's namespace must be launched
    (asserts!
      (is-some (get launched-at namespace-props))
      (err ERR_NAMESPACE_NOT_LAUNCHED))
    ;; The sender must match the name's current owner
    (asserts!
      (is-eq owner tx-sender)
      (err ERR_NAME_OPERATION_UNAUTHORIZED))
    ;; The name must not be in the renewal grace period
    (asserts!
      (is-eq (unwrap! (is-name-in-grace-period? namespace name) (err ERR_PANIC)) false)
      (err ERR_NAME_GRACE_PERIOD))
    ;; The name must not be expired 
    (asserts!
      (is-eq (unwrap! (is-name-lease-expired? namespace name) (err ERR_PANIC)) false)
      (err ERR_NAME_EXPIRED))
    ;; The name must not be revoked
    (asserts!
      (is-none (get revoked-at name-props))
      (err ERR_NAME_REVOKED))
    ;; Update the zonefile
    (update-zonefile-and-props
      namespace 
      name  
      (get registered-at name-props)
      (get imported-at name-props)
      none
      zonefile-hash)
    (ok true)))

;; NAME_TRANSFER
;; A NAME_TRANSFER transaction changes the name's public key hash. You would send one of these transactions if you wanted to:
;; - Change your private key
;; - Send the name to someone else
;; When transferring a name, you have the option to also clear the name's zone file hash (i.e. set it to null). 
;; This is useful for when you send the name to someone else, so the recipient's name does not resolve to your zone file.
(define-public (name-transfer (namespace (buff 20))
                              (name (buff 32))
                              (new-owner principal)
                              (zonefile-hash (optional (buff 20))))
  (let (
    (current-owned-name (map-get? owner-name ((owner new-owner))))
    (namespace-props (unwrap!
      (map-get? namespaces ((namespace namespace)))
      (err ERR_NAMESPACE_NOT_FOUND))))
    (let (
      (owner (unwrap!
        (nft-get-owner? names (tuple (name name) (namespace namespace)))
        (err ERR_NAME_NOT_FOUND))) ;; The name must exist
      (name-props (unwrap!
        (map-get? name-properties ((name name) (namespace namespace)))
        (err ERR_NAME_NOT_FOUND))) ;; The name must exist
      (can-new-owner-get-name (try! (can-register-name new-owner))))
      ;; The namespace must be launched
      (asserts!
        (is-some (get launched-at namespace-props))
        (err ERR_NAMESPACE_NOT_LAUNCHED))
      ;; The sender must match the name's current owner
      (asserts!
        (is-eq owner tx-sender)
        (err ERR_NAME_OPERATION_UNAUTHORIZED))
      ;; The name must not be in the renewal grace period
      (asserts!
        (is-eq (unwrap! (is-name-in-grace-period? namespace name) (err ERR_PANIC)) false)
        (err ERR_NAME_GRACE_PERIOD))
      ;; The name must not be expired
      (asserts!
        (is-eq (unwrap! (is-name-lease-expired? namespace name) (err ERR_PANIC)) false)
        (err ERR_NAME_EXPIRED))
      ;; The name must not be revoked
      (asserts!
        (is-none (get revoked-at name-props))
        (err ERR_NAME_REVOKED))
      ;; The new owner does not own a name
      (asserts!
        can-new-owner-get-name
        (err ERR_PRINCIPAL_ALREADY_ASSOCIATED))
      ;; Transfer the name
      (unwrap!
        (update-name-ownership? namespace name tx-sender new-owner)
        (err ERR_NAME_TRANSFER_FAILED))
      ;; Update or clear the zonefile
      (update-zonefile-and-props
          namespace 
          name  
          (get registered-at name-props)
          (get imported-at name-props)
          none
          (if (is-none zonefile-hash)
            0x
            (unwrap! zonefile-hash (err ERR_PANIC))))
      (ok true))))

;; NAME_REVOKE
;; A NAME_REVOKE transaction makes a name unresolvable. The BNS consensus rules stipulate that once a name 
;; is revoked, no one can change its public key hash or its zone file hash. 
;; The name's zone file hash is set to null to prevent it from resolving.
;; You should only do this if your private key is compromised, or if you want to render your name unusable for whatever reason.
(define-public (name-revoke (namespace (buff 20))
                            (name (buff 32)))
  (let (
    (owner (unwrap!
      (nft-get-owner? names (tuple (name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND))) ;; The name must exist
    (namespace-props (unwrap!
      (map-get? namespaces ((namespace namespace)))
      (err ERR_NAMESPACE_NOT_FOUND)))
    (name-props (unwrap!
      (map-get? name-properties ((name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND)))) ;; The name must exist
    ;; The namespace must be launched
    (asserts!
      (is-some (get launched-at namespace-props))
      (err ERR_NAMESPACE_NOT_LAUNCHED))
    ;; The sender must match the name's current owner
    (asserts!
      (is-eq owner tx-sender)
      (err ERR_NAME_OPERATION_UNAUTHORIZED))
    ;; The name must not be expired
    (asserts!
      (is-eq (unwrap! (is-name-lease-expired? namespace name) (err ERR_PANIC)) false)
      (err ERR_NAME_EXPIRED))
    ;; The name must not be in the renewal grace period
    (asserts!
      (is-eq (unwrap! (is-name-in-grace-period? namespace name) (err ERR_PANIC)) false)
      (err ERR_NAME_GRACE_PERIOD))
    ;; The name must not be revoked
    (asserts!
      (is-none (get revoked-at name-props))
      (err ERR_NAME_REVOKED))
    ;; Clear the zonefile
    (update-zonefile-and-props
        namespace 
        name  
        (get registered-at name-props)
        (get imported-at name-props)
        (some block-height)
        0x)
    (ok true)))

;; NAME_RENEWAL
;; Depending in the namespace rules, a name can expire. For example, names in the .id namespace expire after 2 years. 
;; You need to send a NAME_RENEWAL every so often to keep your name.
;; You will pay the registration cost of your name to the namespace's designated burn address when you renew it.
;; When a name expires, it enters a month-long "grace period" (5000 blocks). 
;; It will stop resolving in the grace period, and all of the above operations will cease to be honored by the BNS consensus rules.
;; You may, however, send a NAME_RENEWAL during this grace period to preserve your name.
;; If your name is in a namespace where names do not expire, then you never need to use this transaction.
(define-public (name-renewal (namespace (buff 20))
                             (name (buff 32))
                             (stx-to-burn uint)
                             (new-owner (optional principal))
                             (zonefile-hash (optional (buff 20))))
  (let (
    (namespace-props (unwrap!
      (map-get? namespaces ((namespace namespace)))
      (err ERR_NAMESPACE_NOT_FOUND)))
    (owner (unwrap!
      (nft-get-owner? names (tuple (name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND))) ;; The name must exist
    (name-props (unwrap!
      (map-get? name-properties ((name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND)))) ;; The name must exist
    ;; The namespace must be launched
    (asserts!
      (is-some (get launched-at namespace-props))
      (err ERR_NAMESPACE_NOT_LAUNCHED))
    ;; The namespace should require renewals
    (asserts!
      (> (get lifetime namespace-props) u0)
      (err ERR_NAME_OPERATION_UNAUTHORIZED))
    ;; The sender must match the name's current owner
    (asserts!
      (is-eq owner tx-sender)
      (err ERR_NAME_OPERATION_UNAUTHORIZED))
    ;; If expired, the name must not be in the renewal grace period.
    (if (unwrap! (is-name-lease-expired? namespace name) (err ERR_PANIC))
      (asserts!
        (is-eq (unwrap! (is-name-in-grace-period? namespace name) (err ERR_PANIC)) true)
        (err ERR_NAME_EXPIRED))
      true)    
    ;; The amount burnt must be equal to or greater than the cost of the namespace
    (asserts!
      (>= stx-to-burn (compute-name-price name (get price-function namespace-props)))
      (err ERR_NAME_STX_BURNT_INSUFFICIENT))
    ;; The name must not be revoked
    (asserts!
      (is-none (get revoked-at name-props))
      (err ERR_NAME_REVOKED))
    ;; Transfer the name, if any new-owner
    (if (is-none new-owner)
      true 
      (try! (can-register-name (unwrap-panic new-owner))))
    ;; Update the zonefile, if any.
    (if (is-none zonefile-hash)
      (map-set name-properties
        ((namespace namespace) (name name))
        ((registered-at (some block-height))
         (imported-at none)
         (revoked-at none)
         (zonefile-hash (get zonefile-hash name-props))))
      (update-zonefile-and-props
              namespace 
              name
              (some block-height)
              none
              none
              (unwrap! zonefile-hash (err ERR_PANIC))))  
    (ok true)))

;; Additionals public methods

(define-read-only (can-register-name (owner principal))
  (let ((current-owned-name (map-get? owner-name ((owner owner)))))
    (if (is-none current-owned-name)
      (ok true)
      (let (
        (namespace (unwrap-panic (get namespace current-owned-name)))
        (name (unwrap-panic (get name current-owned-name))))
        ;; Early return if lease is expired
        (asserts! 
          (not (try! (is-name-lease-expired? namespace name)))
          (ok true))
        (let (
          (name-props (unwrap-panic (map-get? name-properties ((namespace namespace) (name name))))))
          ;; Has name been revoked?
          (asserts! (is-some (get revoked-at name-props)) (ok false))
          (ok true))))))

(define-read-only (can-name-be-registered (namespace (buff 20)) (name (buff 32)))
  (let (
      (wrapped-name-props (map-get? name-properties ((namespace namespace) (name name))))
      (namespace-props (unwrap! (map-get? namespaces ((namespace namespace))) (ok false))))
    ;; Ensure that namespace has been launched 
    (unwrap! (get launched-at namespace-props) (ok false))
    ;; Early return - Name has never be minted
    (asserts! (is-some (nft-get-owner? names (tuple (name name) (namespace namespace)))) (ok true))
    (let ((name-props (unwrap! wrapped-name-props (err ERR_PANIC))))
      ;; Integrity check - Ensure that the name was either "imported" or "registered".
      (asserts! (is-eq (xor 
        (match (get registered-at name-props) res 1 0)
        (match (get imported-at name-props)   res 1 0)) 1) (err ERR_PANIC))
      ;; Is lease expired?
      (is-name-lease-expired? namespace name))))

(define-read-only (name-resolve (namespace (buff 20)) (name (buff 32)))
  (let (
    (owner (unwrap!
      (nft-get-owner? names (tuple (name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND))) ;; The name must exist
    (name-props (unwrap!
      (map-get? name-properties ((name name) (namespace namespace)))
      (err ERR_NAME_NOT_FOUND)))
    (namespace-props (unwrap! 
      (map-get? namespaces ((namespace namespace))) 
      (err ERR_NAMESPACE_NOT_FOUND)))
    (is-lease-expired (is-name-lease-expired? namespace name)))
    ;; If the namespace is already launched
    (if (is-some (get launched-at namespace-props))
      (begin
        ;; The name must not be in the renewal grace period
        (asserts!
          (is-eq (try! (is-name-in-grace-period? namespace name)) false)
          (err ERR_NAME_GRACE_PERIOD))
        ;; The name must not be expired
        (if (is-ok is-lease-expired)
          (asserts! (not (try! is-lease-expired)) (err ERR_NAME_EXPIRED))
          true))        
      true)
    ;; The name must not be revoked
    (asserts!
      (is-none (get revoked-at name-props))
      (err ERR_NAME_REVOKED))
    ;; Get the zonefile
    (ok { zonefile-hash: (get zonefile-hash name-props), owner: owner })))
