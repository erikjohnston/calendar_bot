
window.addEventListener('load', (event) => {
    let options = { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric', hour: "numeric", minute: "numeric"};

    document.querySelectorAll(".datetime")
        .forEach(function(node) {
            node.innerText = new Date(node.innerText).toLocaleString(undefined, options);
        });
  });
