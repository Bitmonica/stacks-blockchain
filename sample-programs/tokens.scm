(define-map tokens ((account principal)) ((balance int)))
(define (get-balance (account principal))
  (default-to 0 (get balance (fetch-entry tokens (tuple (account account))))))

(define (token-credit! (account principal) (tokens int))
  (if (<= tokens 0)
      (err "must move positive balance")
      (let ((current-amount (get-balance account)))
        (begin
          (set-entry! tokens (tuple (account account))
                      (tuple (balance (+ tokens current-amount))))
          (ok tokens)))))

(define-public (token-transfer (to principal) (amount int))
  (let ((balance (get-balance tx-sender)))
    (if (or (> amount balance) (<= amount 0))
        (err "must transfer positive balance and possess funds")
        (begin
          (set-entry! tokens (tuple (account tx-sender))
                      (tuple (balance (- balance amount))))
          (token-credit! to amount)))))

(define-public (mint! (amount int))
   (let ((balance (get-balance tx-sender)))
     (token-credit! tx-sender amount)))

(token-credit! 'SZ2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQ9H6DPR 10000)
(token-credit! 'SM2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQVX8X0G 300)
