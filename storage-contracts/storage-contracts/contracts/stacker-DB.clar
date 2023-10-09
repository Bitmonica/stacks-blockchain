;; Definitions
(define-map signer-slots-by-reward-cycle uint (list 10 {signer: principal, num-slots: uint}))

;;(define-constant pox-info (unwrap-panic (contract-call? .pox-3 get-pox-info)))
(define-constant pox-info {first-burnchain-block-height: u0, reward-cycle-length: u2100})

(define-constant err-unauthorised (err u2000))
(define-constant err-not-in-prepare-phase (err u2001))


;; Cycle helpers
(define-constant first-burn-block-height u666050)
(define-constant normal-cycle-len u2100)
(define-constant prepare-phase-len u100)
(define-read-only (reward-cycle-to-burn-height (cycle uint))
	(+ (get first-burnchain-block-height pox-info) (* cycle (get reward-cycle-length pox-info))))

(define-read-only (burn-height-to-reward-cycle (height uint))
	(/ (- height (get first-burnchain-block-height pox-info)) (get reward-cycle-length pox-info)))

(define-read-only (current-pox-reward-cycle)
	(burn-height-to-reward-cycle burn-block-height))

;; Read
 (define-read-only (stackerdb-get-config)
            (ok {
                chunk-size: u4096,
                write-freq: u0,
                max-writes: u4096,
                max-neighbors: u32,
                hint-replicas: (list )
            }))

(define-read-only (get-current-signer-slots)
    (map-get? signer-slots-by-reward-cycle (current-pox-reward-cycle))
)

(define-read-only (get-any-signer-slots (cycle uint))
    (map-get? signer-slots-by-reward-cycle cycle)
)

;; Write
;; Written to by PoX4 during prepare phase
(define-public (stackerdb-set-next-cycle-signer-slots (slots (list 10 {signer: principal, num-slots: uint})))
    (let    
        (
            (current-cycle (burn-height-to-reward-cycle block-height))
			(previous-cycle (- current-cycle u1))
        )
        ;; Assert currently in prepare phase (< 100 blocks until next cycle)
        (asserts! (> block-height (+ (- normal-cycle-len prepare-phase-len) (reward-cycle-to-burn-height current-cycle))) err-not-in-prepare-phase)

        ;; Assert contract-caller is .pox-4?
        (asserts! (is-eq contract-caller .pox-4) err-unauthorised)

        (ok true)
    )
)